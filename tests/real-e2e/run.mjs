#!/usr/bin/env node
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
const result = {
  schemaVersion: 1,
  tests: {},
};

let adapter;
let temporaryDirectory;

try {
  const spring = parseSpringOpenAi(await readFile(springConfig, 'utf8'));
  const upstreamRoot = stripTrailingSlash(args['upstream-root'] ?? spring.baseUrl);
  const adapterBaseUrl = `${upstreamRoot}${spring.completionsPath.replace(/\/chat\/completions$/, '')}`;
  const completionsUrl = `${upstreamRoot}${spring.completionsPath}`;
  result.model = spring.model;

  result.tests.upstream = await testUpstream(completionsUrl, spring.apiKey, spring.model);
  assert(result.tests.upstream.passed, 'Direct upstream completion failed');

  temporaryDirectory = await mkdtemp(path.join(tmpdir(), 'claude-adapter-real-e2e-'));
  const adapterConfig = path.join(temporaryDirectory, 'config.json');
  await writeFile(
    adapterConfig,
    `${JSON.stringify({
      baseUrl: adapterBaseUrl,
      apiKey: spring.apiKey,
      models: { opus: spring.model, sonnet: spring.model, haiku: spring.model },
    })}\n`,
    { mode: 0o600 }
  );
  adapter = await startAdapter(binary, adapterConfig);

  result.tests.adapter = await testAdapter(adapter.url, spring.model);
  assert(result.tests.adapter.passed, 'Adapter output_config.format scenario failed');

  result.tests.health = await testHealth(adapter.url);
  assert(result.tests.health.passed, 'Adapter health check failed');

  if (withClaude) {
    result.tests.claudeText = await testClaude(
      adapter.url,
      spring.model,
      temporaryDirectory,
      false
    );
    assert(result.tests.claudeText.passed, 'Claude Code text scenario failed');
    result.tests.claudeRead = await testClaude(adapter.url, spring.model, temporaryDirectory, true);
    assert(result.tests.claudeRead.passed, 'Claude Code Read tool scenario failed');
  }

  result.passed = true;
} catch (error) {
  result.passed = false;
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
} finally {
  if (adapter) await adapter.stop();
  if (temporaryDirectory) {
    if (withClaude) await new Promise((resolve) => setTimeout(resolve, 1_000));
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
  await mkdir(resultsDir, { recursive: true });
  const model = String(result.model ?? 'unknown').replace(/[^a-z0-9.-]+/gi, '-');
  const timestamp = startedAt
    .toISOString()
    .replace(/[-:]/g, '')
    .replace(/\.\d{3}Z$/, 'Z');
  const output = path.join(resultsDir, `${timestamp}-${model}.json`);
  await writeFile(output, `${JSON.stringify(result, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify({ result: output, passed: result.passed })}\n`);
}

function parseArgs(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index++) {
    const value = values[index];
    if (!value.startsWith('--')) throw new Error(`Unexpected argument: ${value}`);
    const name = value.slice(2);
    if (name === 'with-claude' || name === 'require-thinking') parsed[name] = true;
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
      messages: [{ role: 'user', content: 'Reply with exactly UPSTREAM_OK' }],
      max_tokens: 256,
      temperature: 0,
      stream: false,
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const body = await response.json();
  const message = body.choices?.[0]?.message ?? {};
  return {
    passed: response.ok && message.content === 'UPSTREAM_OK',
    status: response.status,
    responseModel: body.model ?? null,
    finishReason: body.choices?.[0]?.finish_reason ?? null,
    contentMatched: message.content === 'UPSTREAM_OK',
    reasoningTokens: body.usage?.completion_tokens_details?.reasoning_tokens ?? null,
    inputTokens: body.usage?.prompt_tokens ?? null,
    outputTokens: body.usage?.completion_tokens ?? null,
    errorType: body.error?.type ?? null,
  };
}

async function startAdapter(binary, config) {
  const child = spawn(binary, ['--config', config, '--port', '0'], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', (chunk) => {
    stderr += chunk;
  });
  const url = await new Promise((resolveReady, reject) => {
    let stdout = '';
    let settled = false;
    const finish = (error, readyUrl) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) {
        child.kill('SIGTERM');
        reject(error);
      } else resolveReady(readyUrl);
    };
    const timeout = setTimeout(() => finish(new Error('Adapter startup timed out')), 10_000);
    child.once('error', (error) => finish(error));
    child.once('exit', (code) => finish(new Error(`Adapter exited before ready: ${code}`)));
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      for (const line of stdout.split('\n')) {
        if (!line.startsWith('CLAUDE_ADAPTER_READY=')) continue;
        try {
          const readyUrl = JSON.parse(line.slice('CLAUDE_ADAPTER_READY='.length)).url;
          if (!readyUrl) throw new Error('Adapter ready record has no URL');
          finish(undefined, readyUrl);
        } catch (error) {
          finish(error);
        }
      }
    });
  });
  return {
    url,
    async stop() {
      if (child.exitCode !== null) return;
      child.kill('SIGTERM');
      await new Promise((resolveExit, reject) => {
        const timeout = setTimeout(
          () => reject(new Error(`Adapter shutdown timed out: ${stderr.trim()}`)),
          2_000
        );
        child.once('exit', () => {
          clearTimeout(timeout);
          resolveExit();
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
      max_tokens: requireThinking ? 2048 : 256,
      ...(requireThinking ? { thinking: { type: 'enabled', budget_tokens: 1024 } } : {}),
      temperature: 0,
      stream: true,
      messages: [{ role: 'user', content: 'Reply with exactly ADAPTER_406_FIXED' }],
      metadata: { user_id: 'real-e2e' },
      output_config: {
        effort: 'high',
        format: { type: 'json_schema', schema: { type: 'object' } },
      },
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const eventTypes = new Set();
  const deltaTypes = new Set();
  let firstContentType = null;
  const result = await readAnthropicStream(response, (event) => {
    eventTypes.add(event.type);
    if (event.delta?.type) {
      deltaTypes.add(event.delta.type);
      firstContentType ??= event.delta.type;
    }
  });
  const text = result.blocks.map((block) => block.text ?? '').join('');
  const thinkingObserved = deltaTypes.has('thinking_delta');
  return {
    passed: text === 'ADAPTER_406_FIXED' && (!requireThinking || thinkingObserved),
    status: response.status,
    eventTypes: [...eventTypes],
    deltaTypes: [...deltaTypes],
    firstContentType,
    thinkingObserved,
    contentMatched: text === 'ADAPTER_406_FIXED',
    stopReason: result.stopReason,
    inputTokens: result.usage.input_tokens ?? null,
    outputTokens: result.usage.output_tokens,
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

async function testClaude(adapterUrl, model, directory, toolTest) {
  const expected = toolTest ? 'CLAUDE_READ_OK_20260920' : 'CLAUDE_CODE_OK';
  if (toolTest) await writeFile(path.join(directory, 'truth.txt'), `${expected}\n`);
  const prompt = toolTest
    ? 'Read truth.txt and reply with exactly its contents.'
    : `Reply with exactly ${expected}`;
  const command = [
    '-p',
    prompt,
    '--model',
    'sonnet',
    '--output-format',
    'stream-json',
    '--verbose',
    '--include-partial-messages',
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
  const messages = execution.stdout
    .split('\n')
    .filter(Boolean)
    .flatMap((line) => {
      try {
        return [JSON.parse(line)];
      } catch {
        return [];
      }
    });
  const events = messages
    .filter((message) => message.type === 'stream_event')
    .map((message) => message.event);
  const thinkingObserved = events.some((event) => event.delta?.type === 'thinking_delta');
  const toolObserved = events.some(
    (event) => event.content_block?.type === 'tool_use' && event.content_block.name === 'Read'
  );
  const expectedContentObserved = messages.some(
    (message) =>
      message.type === 'result' &&
      typeof message.result === 'string' &&
      message.result.includes(expected)
  );
  return {
    passed:
      execution.code === 0 &&
      expectedContentObserved &&
      (!requireThinking || thinkingObserved) &&
      (!toolTest || toolObserved),
    exitCode: execution.code,
    expectedContentObserved,
    thinkingObserved,
    toolObserved,
    eventTypes: [...new Set(events.map((event) => event.type))],
  };
}

function runCommand(command, commandArgs, options) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, commandArgs, { ...options, stdio: ['ignore', 'pipe', 'ignore'] });
    let stdout = '';
    let settled = false;
    const finish = (error, execution) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) reject(error);
      else resolveRun(execution);
    };
    const timeout = setTimeout(() => {
      child.kill('SIGTERM');
      finish(new Error(`${command} timed out`));
    }, 180_000);
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
    });
    child.once('error', (error) => finish(error));
    child.once('exit', (code) => {
      finish(undefined, { code, stdout });
    });
  });
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}
