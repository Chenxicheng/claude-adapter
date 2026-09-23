#!/usr/bin/env node
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readAnthropicStream } from '../../bench/stream-validation.mjs';

const args = parseArgs(process.argv.slice(2));
const currentDirectory = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(currentDirectory, '..', '..');
const springConfig = required(args, 'spring-config');
const binary = path.resolve(
  args['adapter-binary'] ?? path.join(root, 'native/target/release/claude-adapter-native')
);
const resultsDir = path.resolve(args['results-dir'] ?? path.join(currentDirectory, 'results'));
const withClaude = args['with-claude'] === true;
const requireThinking = args['require-thinking'] === true;
const startedAt = new Date();
const result = { schemaVersion: 2, tests: {} };
let adapter;
let probe;
let temporaryDirectory;
let credentialVariable;

try {
  const spring = parseSpringOpenAi(await readFile(springConfig, 'utf8'));
  const upstreamRoot = stripTrailingSlash(args['upstream-root'] ?? spring.baseUrl);
  result.model = spring.model;
  credentialVariable = `CLAUDE_ADAPTER_E2E_${process.pid}_${Date.now()}`;
  result.tests.upstream = await testUpstream(
    `${upstreamRoot}${spring.completionsPath}`,
    spring.apiKey,
    spring.model
  );

  temporaryDirectory = await mkdtemp(path.join(tmpdir(), 'claude-adapter-real-e2e-'));
  probe = await startProbe(`${upstreamRoot}${spring.completionsPath}`);
  const adapterConfig = path.join(temporaryDirectory, 'config.json');
  const adapterBaseUrl = `${probe.url}${spring.completionsPath.replace(/\/chat\/completions$/, '')}`;
  await writeFile(
    adapterConfig,
    `${JSON.stringify({ baseUrl: adapterBaseUrl, apiKeyEnv: credentialVariable, models: { opus: spring.model, sonnet: spring.model, haiku: spring.model } })}\n`,
    { mode: 0o600 }
  );
  adapter = await startAdapter(binary, adapterConfig, credentialVariable, spring.apiKey);
  result.tests.adapter = await testAdapter(adapter.url, spring.model);
  assert(result.tests.adapter.passed, 'Adapter streaming probe failed');
  result.tests.health = await testHealth(adapter.url);
  assert(result.tests.health.passed, 'Adapter health check failed');
  if (withClaude) {
    result.tests.claudeText = await testClaude(adapter, spring.model, temporaryDirectory, false);
    result.tests.claudeRead = await testClaude(adapter, spring.model, temporaryDirectory, true);
    assert(result.tests.claudeText.passed, 'Claude Code complex text scenario failed');
    assert(result.tests.claudeRead.passed, 'Claude Code Read tool scenario failed');
  }
  result.probe = probe.summary();
  result.passed =
    result.tests.upstream.passed &&
    result.tests.adapter.passed &&
    result.tests.health.passed &&
    (!withClaude || (result.tests.claudeText.passed && result.tests.claudeRead.passed));
  assert(result.passed, 'Real E2E acceptance failed');
} catch (error) {
  result.passed = false;
  if (probe) result.probe = probe.summary();
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
} finally {
  if (adapter) await adapter.stop();
  if (probe) await probe.stop();
  await mkdir(resultsDir, { recursive: true });
  const model = String(result.model ?? 'unknown').replace(/[^a-z0-9.-]+/gi, '-');
  const timestamp = startedAt
    .toISOString()
    .replace(/[-:]/g, '')
    .replace(/\.\d{3}Z$/, 'Z');
  const output = path.join(resultsDir, `${timestamp}-${model}.json`);
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify({ result: output, passed: result.passed })}\n`);
  if (temporaryDirectory) await rm(temporaryDirectory, { recursive: true, force: true });
}

function parseArgs(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index++) {
    const value = values[index];
    if (!value.startsWith('--')) throw new Error(`Unexpected argument: ${value}`);
    const name = value.slice(2);
    if (name === 'with-claude') parsed[name] = true;
    else parsed[name] = values[++index];
  }
  return parsed;
}
function required(values, name) {
  const value = values[name];
  if (!value || value === true) throw new Error(`--${name} is required`);
  return value;
}
function parseSpringOpenAi(yaml) {
  const scalar = (name) => {
    const matches = [...yaml.matchAll(new RegExp(`^\\s+${name}:\\s*(.+?)\\s*$`, 'gm'))];
    if (matches.length !== 1) throw new Error(`Expected exactly one ${name} in Spring config`);
    return matches[0][1].replace(/^(['"])(.*)\1$/, '$2');
  };
  return {
    apiKey: scalar('api-key'),
    baseUrl: stripTrailingSlash(scalar('base-url')),
    completionsPath: scalar('completions-path'),
    model: scalar('model'),
  };
}
function stripTrailingSlash(value) {
  return value.replace(/\/+$/, '');
}

async function testUpstream(url, apiKey, model) {
  const response = await fetch(url, {
    method: 'POST',
    headers: { authorization: `Bearer ${apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({
      model,
      messages: [
        {
          role: 'user',
          content:
            'Explain five independent streaming boundary cases, compare three repair strategies, and give eight acceptance checks in a detailed answer.',
        },
      ],
      max_tokens: 8192,
      temperature: 0,
      stream: true,
    }),
    signal: AbortSignal.timeout(180_000),
  });
  let buffer = '';
  const decoder = new TextDecoder();
  let eventCount = 0;
  let textDeltaCount = 0;
  let finishReason = null;
  let inputTokens = null;
  let outputTokens = null;
  if (response.body)
    for await (const chunk of response.body) {
      buffer += decoder.decode(chunk, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop();
      for (const line of lines) {
        const dataLine = line.replace(/\r$/, '');
        if (!dataLine.startsWith('data: ') || dataLine.slice(6) === '[DONE]') continue;
        let event;
        try {
          event = JSON.parse(dataLine.slice(6));
        } catch {
          continue;
        }
        eventCount++;
        if (
          typeof event.choices?.[0]?.delta?.content === 'string' &&
          event.choices[0].delta.content.length > 0
        )
          textDeltaCount++;
        finishReason ??= event.choices?.[0]?.finish_reason ?? null;
        inputTokens ??= event.usage?.prompt_tokens ?? null;
        outputTokens ??= event.usage?.completion_tokens ?? null;
      }
    }
  return {
    passed: response.ok && eventCount >= 2 && textDeltaCount >= 2,
    status: response.status,
    stream: true,
    eventCount,
    textDeltaCount,
    finishReason,
    inputTokens,
    outputTokens,
  };
}

async function startProbe(upstreamUrl) {
  const requests = [];
  const server = createServer(async (request, response) => {
    const started = Date.now();
    let body = '';
    for await (const chunk of request) body += chunk;
    let parsed = {};
    try {
      parsed = JSON.parse(body);
    } catch {
      parsed = {};
    }
    const record = {
      model: typeof parsed.model === 'string' ? parsed.model : null,
      stream: parsed.stream === true,
      status: null,
      upstreamFirstByteMs: null,
      upstreamCompletedMs: null,
      upstreamCompletedAt: null,
      requestCompletedMs: null,
    };
    requests.push(record);
    try {
      const upstream = await fetch(upstreamUrl, {
        method: request.method,
        headers: forwardHeaders(request.headers),
        body,
      });
      record.status = upstream.status;
      response.writeHead(
        upstream.status,
        Object.fromEntries(
          [...upstream.headers].filter(([name]) => ['content-type', 'cache-control'].includes(name))
        )
      );
      if (!upstream.body) {
        record.upstreamCompletedMs = Date.now() - started;
        record.upstreamCompletedAt = Date.now();
        response.end();
        return;
      }
      const reader = upstream.body.getReader();
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        record.upstreamFirstByteMs ??= Date.now() - started;
        response.write(Buffer.from(value));
      }
      record.upstreamCompletedMs = Date.now() - started;
      record.upstreamCompletedAt = Date.now();
      response.end();
    } catch {
      record.status = 502;
      response.writeHead(502, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: { type: 'proxy_error' } }));
    } finally {
      record.requestCompletedMs = Date.now() - started;
    }
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  return {
    url: `http://127.0.0.1:${address.port}`,
    count: () => requests.length,
    recordsSince: (index) => requests.slice(index),
    latest: () => requests.at(-1),
    summary() {
      return {
        requestCount: requests.length,
        requests: requests.map(
          ({
            model,
            stream,
            status,
            upstreamFirstByteMs,
            upstreamCompletedMs,
            requestCompletedMs,
          }) => ({
            model,
            stream,
            status,
            upstreamFirstByteMs,
            upstreamCompletedMs,
            requestCompletedMs,
          })
        ),
      };
    },
    async stop() {
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
function forwardHeaders(headers) {
  const forwarded = {};
  for (const [name, value] of Object.entries(headers))
    if (!['host', 'content-length'].includes(name)) forwarded[name] = value;
  return forwarded;
}

async function startAdapter(binaryPath, config, credentialName, credential) {
  const child = spawn(binaryPath, ['--config', config, '--port', '0'], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, [credentialName]: credential },
  });
  let stderrBuffer = '';
  let sent = 0;
  child.stderr.on('data', (chunk) => {
    stderrBuffer += String(chunk);
    const lines = stderrBuffer.split('\n');
    stderrBuffer = lines.pop();
    sent += lines.filter((line) => line.includes('[sent]')).length;
  });
  const url = await new Promise((resolve, reject) => {
    let stdout = '';
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      error ? reject(error) : resolve(value);
    };
    const timeout = setTimeout(() => {
      child.kill('SIGTERM');
      finish(new Error('Adapter startup timed out'));
    }, 10_000);
    child.once('error', (error) => {
      child.kill('SIGTERM');
      finish(error);
    });
    child.once('exit', (code) => finish(new Error(`Adapter exited before ready: ${code}`)));
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      for (const line of stdout.split('\n'))
        if (line.startsWith('CLAUDE_ADAPTER_READY=')) {
          try {
            finish(null, JSON.parse(line.slice('CLAUDE_ADAPTER_READY='.length)).url);
          } catch (error) {
            finish(error);
          }
        }
    });
  });
  return {
    url,
    sentCount: () => sent,
    async stop() {
      if (child.exitCode !== null) return;
      child.kill('SIGTERM');
      await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Adapter shutdown timed out')), 2_000);
        child.once('exit', () => {
          clearTimeout(timeout);
          resolve();
        });
      });
    },
  };
}

async function testAdapter(url, model) {
  const response = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      model,
      max_tokens: 4096,
      temperature: 0,
      stream: true,
      ...(requireThinking ? { thinking: { type: 'enabled', budget_tokens: 1024 } } : {}),
      messages: [
        {
          role: 'user',
          content:
            'Explain five independent streaming boundary cases, compare two repair strategies, and give six acceptance checks in a concise answer.',
        },
      ],
      metadata: { user_id: 'real-e2e' },
      output_config: { format: { type: 'json_schema', schema: { type: 'object' } } },
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const eventTypes = new Set();
  const deltaTypes = new Set();
  const parsed = await readAnthropicStream(response, (event) => {
    eventTypes.add(event.type);
    if (event.delta?.type) deltaTypes.add(event.delta.type);
  });
  const text = parsed.blocks.map((block) => block.text ?? '').join('');
  return {
    passed:
      response.ok &&
      text.length > 0 &&
      deltaTypes.has('text_delta') &&
      (!requireThinking || deltaTypes.has('thinking_delta')),
    status: response.status,
    eventTypes: [...eventTypes],
    deltaTypes: [...deltaTypes],
    stopReason: parsed.stopReason,
    inputTokens: parsed.usage.input_tokens ?? null,
    outputTokens: parsed.usage.output_tokens ?? null,
  };
}
async function testHealth(url) {
  const response = await fetch(`${url}/health`);
  const body = await response.json();
  return {
    passed: response.ok && body.status === 'ok' && body.dropped_jsonl_records === 0,
    status: response.status,
  };
}

async function testClaude(adapter, model, directory, toolTest) {
  const adapterUrl = adapter.url;
  const probeStart = probe.count();
  const rustStart = adapter.sentCount();
  const expected = toolTest ? 'CLAUDE_READ_OK_20260920' : 'CLAUDE_COMPLEX_OK';
  if (toolTest) await writeFile(path.join(directory, 'truth.txt'), `${expected}\n`);
  const prompt = toolTest
    ? 'Read truth.txt and reply with exactly its contents.'
    : 'Analyze a five-part streaming protocol problem, compare three fixes, and provide eight acceptance checks. End your answer with exactly CLAUDE_COMPLEX_OK.';
  const settings = path.join(directory, 'settings.json');
  await writeFile(
    settings,
    `${JSON.stringify({ env: { ANTHROPIC_BASE_URL: adapterUrl, ANTHROPIC_AUTH_TOKEN: 'default', ANTHROPIC_DEFAULT_OPUS_MODEL: model, ANTHROPIC_DEFAULT_SONNET_MODEL: model, ANTHROPIC_DEFAULT_HAIKU_MODEL: model } })}\n`,
    { mode: 0o600 }
  );
  const command = [
    '-p',
    prompt,
    '--model',
    'sonnet',
    '--output-format',
    'stream-json',
    '--verbose',
    '--include-partial-messages',
    '--setting-sources',
    '',
    '--settings',
    settings,
  ];
  if (requireThinking) command.push('--effort', 'high');
  if (toolTest) command.push('--allowedTools', 'Read');
  const execution = await runCommand('claude', command, {
    cwd: directory,
    env: {
      ...process.env,
      ANTHROPIC_BASE_URL: adapterUrl,
      ANTHROPIC_AUTH_TOKEN: 'default',
      ANTHROPIC_DEFAULT_OPUS_MODEL: model,
      ANTHROPIC_DEFAULT_SONNET_MODEL: model,
      ANTHROPIC_DEFAULT_HAIKU_MODEL: model,
    },
  });
  const messages = execution.messages;
  const events = messages
    .filter((message) => message.type === 'stream_event')
    .map((message) => message.event);
  const textTimes = messages
    .filter(
      (message) => message.type === 'stream_event' && message.event?.delta?.type === 'text_delta'
    )
    .map((message) => message.receivedAtMs);
  const caseRequests = probe.recordsSince(probeStart);
  const latest = caseRequests.at(-1);
  const resultMessage = messages.findLast((message) => message.type === 'result');
  const toolObserved = events.some(
    (event) => event.content_block?.type === 'tool_use' && event.content_block.name === 'Read'
  );
  const thinkingObserved = events.some((event) => event.delta?.type === 'thinking_delta');
  const expectedContentObserved =
    typeof resultMessage?.result === 'string' && resultMessage.result.includes(expected);
  const distinctTextDeltas =
    textTimes.length >= 2 &&
    textTimes.some((time, index) => index > 0 && time > textTimes[index - 1]);
  const textBeforeUpstreamDone =
    latest?.upstreamCompletedAt != null &&
    textTimes.some((time) => time < latest.upstreamCompletedAt);
  const rustSent = adapter.sentCount() - rustStart;
  const requestMatched =
    caseRequests.length === rustSent &&
    rustSent >= 1 &&
    caseRequests.every(
      (request) => request.model === model && request.stream === true && request.status === 200
    );
  return {
    passed:
      execution.code === 0 &&
      expectedContentObserved &&
      (!toolTest || toolObserved) &&
      (!requireThinking || thinkingObserved) &&
      distinctTextDeltas &&
      textBeforeUpstreamDone &&
      requestMatched,
    exitCode: execution.code,
    expectedContentObserved,
    toolObserved,
    thinkingObserved,
    eventTypes: [...new Set(events.map((event) => event.type))],
    textDeltaCount: textTimes.length,
    textDeltaFirstMs: textTimes[0] ? textTimes[0] - execution.startedAt : null,
    textDeltaLastMs: textTimes.at(-1) ? textTimes.at(-1) - execution.startedAt : null,
    textDeltaMaxGapMs: maxGap(textTimes),
    textBeforeUpstreamDone,
    upstreamCompletedMs: latest?.upstreamCompletedMs ?? null,
    requestCount: caseRequests.length,
    requestStream: latest?.stream ?? null,
    requestStatus: latest?.status ?? null,
    rustSent,
    requestMatched,
  };
}

function runCommand(command, commandArgs, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, commandArgs, { ...options, stdio: ['ignore', 'pipe', 'ignore'] });
    const startedAt = Date.now();
    let buffer = '';
    const messages = [];
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      error ? reject(error) : resolve(value);
    };
    const timeout = setTimeout(() => {
      child.kill('SIGTERM');
      finish(new Error(`${command} timed out`));
    }, 300_000);
    child.stdout.on('data', (chunk) => {
      buffer += chunk;
      const lines = buffer.split('\n');
      buffer = lines.pop();
      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          messages.push({ ...JSON.parse(line), receivedAtMs: Date.now() });
        } catch {
          /* progress output */
        }
      }
    });
    child.once('error', (error) => finish(error));
    child.once('exit', (code) => {
      if (buffer.trim()) {
        try {
          messages.push({ ...JSON.parse(buffer), receivedAtMs: Date.now() });
        } catch {
          /* progress output */
        }
      }
      finish(null, { code, messages, startedAt });
    });
  });
}
function maxGap(values) {
  return values
    .slice(1)
    .reduce((maximum, value, index) => Math.max(maximum, value - values[index]), 0);
}
function assert(condition, message) {
  if (!condition) throw new Error(message);
}
