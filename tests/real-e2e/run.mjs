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
const defaultUpstreamRoot = 'http://127.0.0.1:1234';
const startedAt = new Date();
const result = {
  schemaVersion: 3,
  upstreamCapabilities: { assistantPrefill: 'native' },
  tests: {},
};
let adapter;
let probe;
let temporaryDirectory;
let credentialVariable;

try {
  const spring = parseSpringOpenAi(await readFile(springConfig, 'utf8'));
  const upstreamRoot = stripTrailingSlash(args['upstream-root'] ?? defaultUpstreamRoot);
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
    `${JSON.stringify({ baseUrl: adapterBaseUrl, apiKeyEnv: credentialVariable, upstreamCapabilities: result.upstreamCapabilities, models: { opus: spring.model, sonnet: spring.model, haiku: spring.model } })}\n`,
    { mode: 0o600 }
  );
  adapter = await startAdapter(binary, adapterConfig, credentialVariable, spring.apiKey);
  result.tests.adapter = await testAdapter(adapter.url, spring.model);
  assert(result.tests.adapter.passed, 'Adapter streaming probe failed');
  result.tests.health = await testHealth(adapter.url);
  assert(result.tests.health.passed, 'Adapter health check failed');
  if (withClaude) {
    const textProbeStart = probe.count();
    result.tests.claudeText = await testClaude(adapter, spring.model, temporaryDirectory, false);
    const exactTextRequest = probe.recordsSince(textProbeStart).at(0)?.body;
    assert(exactTextRequest, 'No converted Claude text request was captured');
    result.tests.matrix = {
      exactTextNonStream: await testOpenAiRequest(
        `${upstreamRoot}${spring.completionsPath}`,
        spring.apiKey,
        { ...exactTextRequest, stream: false, stream_options: undefined },
        { expectedText: 'CLAUDE_COMPLEX_OK' }
      ),
      readAuto: await testOpenAiRequest(
        `${upstreamRoot}${spring.completionsPath}`,
        spring.apiKey,
        minimalReadRequest(spring.model, 'auto'),
        { expectedTool: 'Read' }
      ),
      readNamed: await testOpenAiRequest(
        `${upstreamRoot}${spring.completionsPath}`,
        spring.apiKey,
        minimalReadRequest(spring.model, 'named'),
        { expectedTool: 'Read' }
      ),
    };
    result.tests.claudeRead = await testClaude(adapter, spring.model, temporaryDirectory, true);
    assert(result.tests.claudeText.passed, 'Claude Code complex text scenario failed');
    assert(result.tests.matrix.readAuto.passed, 'Read tool auto-choice scenario failed');
    assert(result.tests.claudeRead.passed, 'Claude Code Read tool scenario failed');
  }
  result.probe = probe.summary();
  result.passed =
    result.tests.upstream.passed &&
    result.tests.adapter.passed &&
    result.tests.health.passed &&
    (!withClaude ||
      (result.tests.claudeText.passed &&
        result.tests.matrix.readAuto.passed &&
        result.tests.claudeRead.passed));
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
    if (['with-claude', 'require-thinking'].includes(name)) parsed[name] = true;
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

function minimalReadRequest(model, choice) {
  return {
    model,
    messages: [{ role: 'user', content: 'Use the Read tool with path truth.txt.' }],
    max_tokens: 1024,
    temperature: 0,
    stream: true,
    stream_options: { include_usage: true },
    tools: [
      {
        type: 'function',
        function: {
          name: 'Read',
          description: 'Read a file from the current working directory.',
          parameters: {
            type: 'object',
            properties: { path: { type: 'string' } },
            required: ['path'],
          },
        },
      },
    ],
    tool_choice: choice === 'named' ? { type: 'function', function: { name: 'Read' } } : 'auto',
  };
}

async function testOpenAiRequest(url, apiKey, request, expected) {
  assert(request && typeof request === 'object', 'No converted Claude text request was captured');
  const response = await fetch(url, {
    method: 'POST',
    headers: { authorization: `Bearer ${apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify(request),
    signal: AbortSignal.timeout(180_000),
  });
  const observation = newUpstreamObservation();
  const toolNames = new Map();
  let expectedTextObserved = expected.expectedText == null;
  let expectedToolObserved = expected.expectedTool == null;
  const inspect = (value) => {
    for (const choice of Array.isArray(value?.choices) ? value.choices : []) {
      const delta = choice?.delta ?? choice?.message;
      if (
        expected.expectedText != null &&
        typeof delta?.content === 'string' &&
        delta.content.includes(expected.expectedText)
      )
        expectedTextObserved = true;
      if (expected.expectedTool != null && Array.isArray(delta?.tool_calls)) {
        for (const call of delta.tool_calls) {
          const index = Number.isInteger(call?.index) ? call.index : 0;
          const name = call?.function?.name;
          if (typeof name === 'string')
            toolNames.set(index, `${toolNames.get(index) ?? ''}${name}`);
        }
        expectedToolObserved ||= [...toolNames.values()].includes(expected.expectedTool);
      }
    }
  };
  if (request.stream) {
    const decoder = new TextDecoder();
    let buffer = '';
    const inspectLine = (line) => {
      const data = line.replace(/\r$/, '').replace(/^data:\s*/, '');
      if (!data || data === '[DONE]') {
        if (data === '[DONE]') observation.doneObserved = true;
        return;
      }
      try {
        const value = JSON.parse(data);
        inspect(value);
        observeOpenAiObject(observation, value);
      } catch {
        observation.outcome = 'protocol_error';
      }
    };
    if (response.body)
      for await (const chunk of response.body) {
        observation.responseChunkCount++;
        buffer += decoder.decode(chunk, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop();
        for (const line of lines) inspectLine(line);
      }
    buffer += decoder.decode();
    if (buffer) inspectLine(buffer);
  } else {
    try {
      observation.responseChunkCount = 1;
      const value = await response.json();
      inspect(value);
      observeOpenAiObject(observation, value);
    } catch {
      observation.outcome = 'protocol_error';
    }
  }
  finalizeObservation(observation);
  return {
    passed:
      response.ok &&
      observation.outcome === 'usable' &&
      expectedTextObserved &&
      expectedToolObserved,
    status: response.status,
    stream: request.stream === true,
    ...observation,
    expectedTextObserved,
    expectedToolObserved,
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
      sequence: requests.length + 1,
      ...summarizeRequest(parsed),
      status: null,
      upstreamFirstByteMs: null,
      upstreamCompletedMs: null,
      upstreamCompletedAt: null,
      requestCompletedMs: null,
      ...newUpstreamObservation(),
    };
    Object.defineProperty(record, 'body', { value: parsed });
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
        record.outcome = 'protocol_error';
        record.upstreamCompletedMs = Date.now() - started;
        record.upstreamCompletedAt = Date.now();
        response.end();
        return;
      }
      const reader = upstream.body.getReader();
      const decoder = new TextDecoder();
      let responseBuffer = '';
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        record.upstreamFirstByteMs ??= Date.now() - started;
        record.responseChunkCount++;
        responseBuffer += decoder.decode(value, { stream: true });
        const lines = responseBuffer.split('\n');
        responseBuffer = lines.pop();
        for (const line of lines) observeSseLine(record, line);
        response.write(Buffer.from(value));
      }
      responseBuffer += decoder.decode();
      if (responseBuffer) observeSseLine(record, responseBuffer);
      finalizeObservation(record);
      record.upstreamCompletedMs = Date.now() - started;
      record.upstreamCompletedAt = Date.now();
      response.end();
    } catch {
      record.status = 502;
      record.outcome = 'protocol_error';
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
        requests: requests.map(({ upstreamCompletedAt: _completedAt, ...record }) => record),
      };
    },
    async stop() {
      await new Promise((resolve) => server.close(resolve));
    },
  };
}

function summarizeRequest(request) {
  const contentTypes = {};
  const messages = Array.isArray(request.messages) ? request.messages : [];
  for (const message of messages) {
    if (typeof message.content === 'string') increment(contentTypes, 'string');
    else if (message.content == null) increment(contentTypes, 'null');
    else if (Array.isArray(message.content))
      for (const part of message.content)
        increment(contentTypes, typeof part?.type === 'string' ? part.type : 'invalid');
    else increment(contentTypes, 'invalid');
  }
  const maxTokenField =
    request.max_completion_tokens != null
      ? 'max_completion_tokens'
      : request.max_tokens != null
        ? 'max_tokens'
        : null;
  const choice = request.tool_choice;
  const lastMessage = messages.at(-1);
  return {
    model: typeof request.model === 'string' ? request.model : null,
    stream: request.stream === true,
    messageCount: messages.length,
    messageRoles: messages.map((message) => message?.role ?? 'invalid'),
    messageContentTypes: contentTypes,
    toolCount: Array.isArray(request.tools) ? request.tools.length : 0,
    toolChoiceType:
      typeof choice === 'string'
        ? choice
        : typeof choice?.type === 'string'
          ? choice.type
          : choice?.function
            ? 'function'
            : null,
    maxTokenField,
    maxTokenValue: maxTokenField ? request[maxTokenField] : null,
    thinkingPresent: ['thinking', 'enable_thinking', 'reasoning_effort'].some(
      (field) => request[field] != null
    ),
    terminalAssistantPrefill:
      lastMessage?.role === 'assistant' &&
      ((typeof lastMessage.content === 'string' && lastMessage.content.length > 0) ||
        (Array.isArray(lastMessage.content) && lastMessage.content.length > 0)),
  };
}

function newUpstreamObservation() {
  return {
    responseChunkCount: 0,
    sseEventCount: 0,
    doneObserved: false,
    finishReasons: [],
    usage: null,
    deltaFieldCounts: {},
    unknownDeltaFields: [],
    textObserved: false,
    reasoningObserved: false,
    refusalObserved: false,
    toolCallObserved: false,
    outcome: null,
  };
}

function observeSseLine(observation, line) {
  const value = line.replace(/\r$/, '');
  if (!value.startsWith('data:')) return;
  const data = value.slice(5).trimStart();
  if (data === '[DONE]') {
    observation.doneObserved = true;
    return;
  }
  try {
    observeOpenAiObject(observation, JSON.parse(data));
  } catch {
    // The adapter owns malformed-SSE reporting; the probe only records a safe outcome.
    observation.outcome = 'protocol_error';
  }
}

function observeOpenAiObject(observation, value) {
  observation.sseEventCount++;
  if (value?.usage && typeof value.usage === 'object') {
    observation.usage = Object.fromEntries(
      Object.entries(value.usage).filter(([, count]) => Number.isFinite(count))
    );
  }
  for (const choice of Array.isArray(value?.choices) ? value.choices : []) {
    if (
      typeof choice?.finish_reason === 'string' &&
      !observation.finishReasons.includes(choice.finish_reason)
    )
      observation.finishReasons.push(choice.finish_reason);
    observeDelta(observation, choice?.delta ?? choice?.message);
  }
}

function observeDelta(observation, delta) {
  if (!delta || typeof delta !== 'object') return;
  const known = new Set([
    'role',
    'content',
    'refusal',
    'reasoning_content',
    'reasoning',
    'tool_calls',
  ]);
  for (const [field, value] of Object.entries(delta)) {
    if (value != null) increment(observation.deltaFieldCounts, field);
    if (!known.has(field) && !observation.unknownDeltaFields.includes(field))
      observation.unknownDeltaFields.push(field);
  }
  observation.textObserved ||= typeof delta.content === 'string' && delta.content.length > 0;
  observation.reasoningObserved ||=
    (typeof delta.reasoning_content === 'string' && delta.reasoning_content.length > 0) ||
    (typeof delta.reasoning === 'string' && delta.reasoning.length > 0);
  observation.refusalObserved ||= typeof delta.refusal === 'string' && delta.refusal.length > 0;
  observation.toolCallObserved ||= Array.isArray(delta.tool_calls) && delta.tool_calls.length > 0;
}

function finalizeObservation(observation) {
  if (observation.outcome === 'protocol_error') return;
  if (observation.finishReasons.length === 0 && !observation.doneObserved) {
    observation.outcome = 'protocol_error';
    return;
  }
  const usable =
    observation.textObserved ||
    observation.reasoningObserved ||
    observation.refusalObserved ||
    observation.toolCallObserved;
  observation.outcome = usable
    ? 'usable'
    : observation.finishReasons.includes('stop')
      ? 'empty_end_turn'
      : 'protocol_error';
  observation.unknownDeltaFields.sort();
}

function increment(counts, name) {
  counts[name] = (counts[name] ?? 0) + 1;
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
  const outputRequest = caseRequests.findLast((request) => request.outcome === 'usable') ?? latest;
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
    outputRequest?.upstreamCompletedAt != null &&
    textTimes.some((time) => time < outputRequest.upstreamCompletedAt);
  const emptyEndTurnObserved = caseRequests.some((request) => request.outcome === 'empty_end_turn');
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
      !emptyEndTurnObserved &&
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
    emptyEndTurnObserved,
    upstreamCompletedMs: latest?.upstreamCompletedMs ?? null,
    requestCount: caseRequests.length,
    requestStream: latest?.stream ?? null,
    requestStatus: latest?.status ?? null,
    rustSent,
    requestMatched,
    requests: caseRequests.map((request) => ({
      sequence: request.sequence,
      outcome: request.outcome,
      finishReasons: request.finishReasons,
      textObserved: request.textObserved,
      reasoningObserved: request.reasoningObserved,
      toolCallObserved: request.toolCallObserved,
    })),
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
