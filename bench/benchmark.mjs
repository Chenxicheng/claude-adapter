#!/usr/bin/env node
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import assert from 'node:assert/strict';
import { readAnthropicStream } from './stream-validation.mjs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const target = process.env.BENCH_TARGET ?? 'rust';
const repetitions = Number(process.env.BENCH_REPETITIONS ?? 5);
const upstreamDelayMs = Number(process.env.BENCH_UPSTREAM_DELAY_MS ?? 0);
const concurrencies = [1, 10, 50, 100];
const workload = process.env.BENCH_WORKLOAD ?? 'text';
if (!['text', 'reasoning-tool'].includes(workload)) throw new Error('Unknown BENCH_WORKLOAD');
const payload = {
  model: 'benchmark-model',
  max_tokens: 128,
  messages: [
    { role: 'user', content: workload === 'text' ? 'Reply with benchmark.' : '__reasoning_tool__' },
  ],
  ...(workload === 'reasoning-tool'
    ? { tools: [{ name: 'lookup', input_schema: { type: 'object' } }] }
    : {}),
};

const mock = await startMockUpstream();
const adapter =
  target === 'node' ? await startNodeAdapter(mock.url) : await startRustAdapter(mock.url);

try {
  if (target === 'rust') await verifyProtocolScenarios(adapter.url, mock);
  await runRequests(adapter.url, 20, 5, false);
  const results = [];
  for (const concurrency of concurrencies) {
    for (let repetition = 1; repetition <= repetitions; repetition++) {
      const total = Math.max(100, concurrency * 10);
      results.push({
        target,
        scenario: 'non-streaming',
        concurrency,
        repetition,
        ...(await runRequests(adapter.url, total, concurrency, false)),
      });
      results.push({
        target,
        scenario: 'streaming',
        concurrency,
        repetition,
        ...(await runRequests(adapter.url, total, concurrency, true)),
      });
    }
  }
  if (target === 'rust') await verifyActiveStreamShutdown(adapter);
  const outputResults = process.env.BENCH_SUMMARY === '1' ? summarize(results) : results;
  process.stdout.write(
    `${JSON.stringify(
      {
        generatedAt: new Date().toISOString(),
        machine: { platform: process.platform, arch: process.arch, cpus: os.cpus().length },
        target,
        repetitions,
        upstreamDelayMs,
        workload,
        results: outputResults,
      },
      null,
      2
    )}\n`
  );
} finally {
  await adapter.stop();
  await mock.stop();
}

function summarize(results) {
  return concurrencies.flatMap((concurrency) =>
    ['non-streaming', 'streaming'].map((scenario) => {
      const samples = results.filter(
        (result) => result.concurrency === concurrency && result.scenario === scenario
      );
      const median = (field) => {
        const values = samples.map((sample) => sample[field]).sort((left, right) => left - right);
        return values[Math.floor(values.length / 2)];
      };
      return {
        target,
        scenario,
        concurrency,
        throughput: median('throughput'),
        p50Ms: median('p50Ms'),
        p95Ms: median('p95Ms'),
        p99Ms: median('p99Ms'),
        firstContentP50Ms: median('firstContentP50Ms'),
        firstThinkingP50Ms: median('firstThinkingP50Ms'),
        firstToolP50Ms: median('firstToolP50Ms'),
        firstTextP50Ms: median('firstTextP50Ms'),
        adapterPeakRssBytes:
          Math.max(...samples.map((sample) => sample.adapterPeakRssBytes ?? 0)) || null,
        errors: samples.reduce((total, sample) => total + sample.errors, 0),
      };
    })
  );
}
process.exit(0);

async function runRequests(url, total, concurrency, streaming) {
  const latencies = [];
  const firstContent = [];
  const thinkingLatencies = [];
  const toolLatencies = [];
  const textLatencies = [];
  let peakRss = 0;
  let sampling = false;
  const sampleMemory = async () => {
    if (sampling) return;
    sampling = true;
    try {
      peakRss = Math.max(peakRss, (await adapterRss(adapter.pid)) ?? 0);
    } finally {
      sampling = false;
    }
  };
  await sampleMemory();
  const sampler = setInterval(sampleMemory, 1000);
  let next = 0;
  let errors = 0;
  let firstError = null;
  const started = performance.now();
  const workers = Array.from({ length: concurrency }, async () => {
    while (next < total) {
      next++;
      const requestStarted = performance.now();
      try {
        const response = await fetch(`${url}/v1/messages`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ ...payload, stream: streaming }),
        });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        if (streaming) {
          const seen = new Set();
          const result = await readAnthropicStream(response, (event) => {
            const kind = event.delta?.type;
            const samples = {
              thinking_delta: thinkingLatencies,
              input_json_delta: toolLatencies,
              text_delta: textLatencies,
            }[kind];
            if (samples && !seen.has(kind)) {
              const elapsed = performance.now() - requestStarted;
              if (seen.size === 0) firstContent.push(elapsed);
              seen.add(kind);
              samples.push(elapsed);
            }
          });
          checkContent(result.blocks);
          assert.equal(result.usage.input_tokens, 10);
        } else {
          const body = await response.json();
          checkContent(body.content);
          assert.equal(body.usage.input_tokens, 10);
          firstContent.push(performance.now() - requestStarted);
        }
        latencies.push(performance.now() - requestStarted);
      } catch (error) {
        errors++;
        firstError ??= error.message;
      }
    }
  });
  try {
    await Promise.all(workers);
  } finally {
    clearInterval(sampler);
  }
  const durationMs = performance.now() - started;
  await sampleMemory();
  latencies.sort((left, right) => left - right);
  firstContent.sort((left, right) => left - right);
  return {
    requests: total,
    errors,
    firstError,
    durationMs,
    throughput: (total - errors) / (durationMs / 1000),
    p50Ms: percentile(latencies, 0.5),
    p95Ms: percentile(latencies, 0.95),
    p99Ms: percentile(latencies, 0.99),
    firstContentP50Ms: percentile(firstContent, 0.5),
    firstThinkingP50Ms: percentile(
      thinkingLatencies.sort((a, b) => a - b),
      0.5
    ),
    firstToolP50Ms: percentile(
      toolLatencies.sort((a, b) => a - b),
      0.5
    ),
    firstTextP50Ms: percentile(
      textLatencies.sort((a, b) => a - b),
      0.5
    ),
    adapterPeakRssBytes: peakRss || null,
    harnessRssBytes: process.memoryUsage().rss,
  };
}

function checkContent(blocks) {
  if (workload === 'text') {
    assert.equal(blocks.map((block) => block.text ?? '').join(''), 'benchmark');
  } else {
    assert.equal(blocks.map((block) => block.thinking ?? '').join(''), 'think');
    const tool = blocks.find((block) => block.type === 'tool_use');
    assert.equal(tool?.name, 'lookup');
    assert.deepEqual(tool.input, { id: 1 });
  }
}

async function adapterRss(pid) {
  try {
    const run = promisify(execFile);
    if (process.platform === 'win32') {
      const { stdout } = await run('tasklist', ['/FI', `PID eq ${pid}`, '/FO', 'CSV', '/NH']);
      const value = stdout
        .trim()
        .split('","')
        .at(-1)
        ?.replace(/[^0-9]/g, '');
      return value ? Number(value) * 1024 : null;
    }
    const { stdout } = await run('ps', ['-o', 'rss=', '-p', String(pid)]);
    return Number(stdout.trim()) * 1024;
  } catch {
    return null;
  }
}

async function verifyProtocolScenarios(url, mock) {
  const firstChatResponse = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      model: 'benchmark-model',
      max_tokens: 128,
      system: 'Base instruction.',
      messages: [
        { role: 'user', content: 'Reply with benchmark.' },
        { role: 'system', content: 'Be concise.' },
      ],
      metadata: { user_id: 'benchmark-user' },
      output_config: { effort: 'high' },
    }),
  });
  if (!firstChatResponse.ok) throw new Error('First-chat compatibility preflight failed');
  await firstChatResponse.json();
  const firstChat = mock.requests.at(-1);
  const firstChatHeaders = mock.requestHeaders.at(-1);
  if (
    firstChat.messages[0]?.role !== 'system' ||
    firstChat.messages[0]?.content !== 'Base instruction.' ||
    firstChat.messages[1]?.role !== 'user' ||
    firstChat.messages[2]?.role !== 'assistant' ||
    firstChat.messages[2]?.content !== 'Be concise.' ||
    firstChat.messages.some((message, index) => index > 0 && message.role === 'system') ||
    firstChat.max_tokens !== 128 ||
    firstChat.stream !== false ||
    'max_completion_tokens' in firstChat ||
    'safety_identifier' in firstChat ||
    'reasoning_effort' in firstChat
  ) {
    throw new Error('First-chat request conversion preflight failed');
  }
  if (
    firstChatHeaders.accept !== 'application/json' ||
    firstChatHeaders['content-type'] !== 'application/json' ||
    firstChatHeaders.authorization !== 'Bearer benchmark' ||
    firstChatHeaders['user-agent'] !== 'Claude-Adapter-Benchmark/2.0' ||
    firstChatHeaders['http-referer'] !== 'https://example.com' ||
    firstChatHeaders['x-title'] !== 'Claude Adapter Benchmark'
  ) {
    throw new Error('TypeScript-compatible HTTP headers preflight failed');
  }

  const toolResponse = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      model: 'benchmark-model',
      max_tokens: 128,
      messages: [{ role: 'user', content: '__reasoning_tool__' }],
      tools: [
        { name: 'lookup', description: 'Lookup', input_schema: { type: 'object' }, strict: true },
      ],
      tool_choice: { type: 'auto', disable_parallel_tool_use: true },
      output_config: {
        effort: 'high',
        format: { type: 'json_schema', schema: { type: 'object' } },
      },
    }),
  });
  const toolBody = await toolResponse.json();
  if (
    !toolResponse.ok ||
    toolBody.content.map((part) => part.type).join(',') !== 'thinking,tool_use'
  ) {
    throw new Error('Reasoning/tool parity preflight failed');
  }
  const toolRequest = mock.requests.at(-1);
  if (!toolRequest.tools?.length) throw new Error('Tool request conversion preflight failed');
  if (
    'strict' in toolRequest.tools[0].function ||
    'parallel_tool_calls' in toolRequest ||
    'response_format' in toolRequest
  ) {
    throw new Error('TypeScript-compatible tool request shape preflight failed');
  }

  const errorResponse = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...payload, model: 'benchmark-error' }),
  });
  const errorBody = await errorResponse.json();
  if (errorResponse.status !== 429 || errorBody.error?.type !== 'rate_limit_error') {
    throw new Error('Upstream error parity preflight failed');
  }

  const interruptedResponse = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      ...payload,
      stream: true,
      messages: [{ role: 'user', content: '__stream_error__' }],
    }),
  });
  if (!(await interruptedResponse.text()).includes('event: error')) {
    throw new Error('Upstream stream interruption preflight failed');
  }

  const disconnectedBefore = mock.disconnectCount();
  const controller = new AbortController();
  const longResponse = await fetch(`${url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    signal: controller.signal,
    body: JSON.stringify({
      ...payload,
      stream: true,
      messages: [{ role: 'user', content: '__long_stream__' }],
    }),
  });
  await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
  if (mock.longChunksSent() === 1000) {
    throw new Error('Slow downstream reader did not apply upstream backpressure');
  }
  const reader = longResponse.body.getReader();
  controller.abort();
  await reader.cancel().catch(() => {});
  const deadline = Date.now() + 1000;
  while (mock.disconnectCount() === disconnectedBefore && Date.now() < deadline) {
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 10));
  }
  if (mock.disconnectCount() === disconnectedBefore) {
    throw new Error('Downstream disconnect did not cancel the upstream stream');
  }
}

async function verifyActiveStreamShutdown(adapter) {
  const response = await fetch(`${adapter.url}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      ...payload,
      stream: true,
      messages: [{ role: 'user', content: '__shutdown_stream__' }],
    }),
  });
  const drain = readAnthropicStream(response);
  await adapter.stop();
  const result = await drain;
  assert.equal(result.blocks.map((block) => block.text ?? '').join(''), 'before shutdown');
  assert.equal(result.stopReason, 'end_turn');
}

function percentile(values, percentileValue) {
  if (values.length === 0) return null;
  return values[Math.min(values.length - 1, Math.floor(values.length * percentileValue))];
}

async function startMockUpstream() {
  const requests = [];
  const requestHeaders = [];
  let disconnects = 0;
  let longChunks = 0;
  const server = http.createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    requests.push(body);
    requestHeaders.push(request.headers);
    if (upstreamDelayMs > 0) {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, upstreamDelayMs));
    }
    if (body.stream && body.messages.at(-1)?.content === '__stream_error__') {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-error',
          model: 'mock-model',
          choices: [{ index: 0, delta: { content: 'before error' }, finish_reason: null }],
        })}\n\n`
      );
      setTimeout(() => response.destroy(new Error('mock stream interrupted')), 10);
      return;
    }
    if (body.stream && body.messages.at(-1)?.content === '__long_stream__') {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const event = `data: ${JSON.stringify({
        id: 'chatcmpl-long',
        model: 'mock-model',
        choices: [{ index: 0, delta: { content: 'x'.repeat(64 * 1024) }, finish_reason: null }],
      })}\n\n`;
      let closed = false;
      response.once('close', () => {
        closed = true;
        disconnects++;
      });
      for (let index = 0; index < 1000 && !closed; index++) {
        const writable = response.write(event);
        longChunks = index + 1;
        if (!writable) await waitForDrainOrClose(response);
      }
      if (!closed) response.end('data: [DONE]\n\n');
      return;
    }
    if (body.stream && body.messages.at(-1)?.content === '__shutdown_stream__') {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-shutdown',
          model: 'mock-model',
          choices: [{ index: 0, delta: { content: 'before shutdown' }, finish_reason: 'stop' }],
        })}\n\n`
      );
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
      response.end('data: [DONE]\n\n');
      return;
    }
    if (body.stream && body.messages.at(-1)?.content === '__reasoning_tool__') {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      for (const chunk of [
        { choices: [{ delta: { reasoning_content: 'think' }, finish_reason: null }] },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_bench',
                    type: 'function',
                    function: { name: 'lookup', arguments: '{"id":' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        {
          choices: [
            {
              delta: { tool_calls: [{ index: 0, function: { arguments: '1}' } }] },
              finish_reason: 'tool_calls',
            },
          ],
        },
        { choices: [], usage: { prompt_tokens: 10, completion_tokens: 3 } },
      ])
        response.write(`data: ${JSON.stringify(chunk)}\n\n`);
      response.end('data: [DONE]\n\n');
      return;
    }
    if (body.stream) {
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-benchmark',
          model: 'mock-model',
          choices: [{ index: 0, delta: { content: 'benchmark' }, finish_reason: 'stop' }],
        })}\n\n`
      );
      response.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-benchmark',
          model: 'mock-model',
          choices: [],
          usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 },
        })}\n\n`
      );
      response.end('data: [DONE]\n\n');
      return;
    }
    if (body.model === 'benchmark-error') {
      response.writeHead(429, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: { message: 'rate limited' } }));
      return;
    }
    if (body.messages.at(-1)?.content === '__reasoning_tool__') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(
        JSON.stringify({
          id: 'chatcmpl-tool',
          object: 'chat.completion',
          model: 'mock-model',
          choices: [
            {
              index: 0,
              message: {
                role: 'assistant',
                reasoning_content: 'think',
                content: null,
                tool_calls: [
                  {
                    id: 'toolu_1',
                    type: 'function',
                    function: { name: 'lookup', arguments: '{"id":1}' },
                  },
                ],
              },
              finish_reason: 'tool_calls',
            },
          ],
          usage: { prompt_tokens: 10, completion_tokens: 3, total_tokens: 13 },
        })
      );
      return;
    }
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(
      JSON.stringify({
        id: 'chatcmpl-benchmark',
        object: 'chat.completion',
        model: 'mock-model',
        choices: [
          { index: 0, message: { role: 'assistant', content: 'benchmark' }, finish_reason: 'stop' },
        ],
        usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 },
      })
    );
  });
  await listen(server);
  const port = server.address().port;
  return {
    url: `http://127.0.0.1:${port}/v1`,
    requests,
    requestHeaders,
    disconnectCount: () => disconnects,
    longChunksSent: () => longChunks,
    stop: () => close(server),
  };
}

function waitForDrainOrClose(response) {
  return new Promise((resolveWait) => {
    const done = () => {
      response.off('drain', done);
      response.off('close', done);
      resolveWait();
    };
    response.once('drain', done);
    response.once('close', done);
  });
}

async function startRustAdapter(baseUrl) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'claude-adapter-bench-'));
  const config = path.join(directory, 'config.json');
  await writeFile(
    config,
    JSON.stringify({
      baseUrl,
      apiKeyEnv: 'CLAUDE_ADAPTER_BENCH_API_KEY',
      models: { opus: 'benchmark-model', sonnet: 'benchmark-model', haiku: 'benchmark-model' },
      upstreamHeaders: {
        'HTTP-Referer': 'https://example.com',
        'X-Title': 'Claude Adapter Benchmark',
        'User-Agent': 'Claude-Adapter-Benchmark/2.0',
      },
    })
  );
  const child = spawn(
    path.resolve(
      process.env.BENCH_BINARY ??
        `native/target/release/claude-adapter-native${process.platform === 'win32' ? '.exe' : ''}`
    ),
    ['--config', config, '--port', '0'],
    {
      stdio: ['pipe', 'pipe', process.env.BENCH_DEBUG ? 'inherit' : 'ignore'],
      env: {
        ...process.env,
        CLAUDE_ADAPTER_BENCH_API_KEY: 'benchmark',
        CLAUDE_ADAPTER_STDIN_SHUTDOWN: '1',
      },
    }
  );
  const url = await new Promise((resolveReady, reject) => {
    let output = '';
    child.once('error', reject);
    child.stdout.on('data', (chunk) => {
      output += chunk;
      const line = output.split('\n').find((value) => value.startsWith('CLAUDE_ADAPTER_READY='));
      if (line) resolveReady(JSON.parse(line.slice('CLAUDE_ADAPTER_READY='.length)).url);
    });
    child.once('exit', (code) => reject(new Error(`Rust adapter exited before ready: ${code}`)));
  });
  let stopped = false;
  return {
    url,
    pid: child.pid,
    stop: async () => {
      if (stopped) return;
      stopped = true;
      const exited = new Promise((resolveExit) => child.once('exit', resolveExit));
      child.stdin.end();
      await exited;
      await rm(directory, { recursive: true, force: true });
    },
  };
}

async function startNodeAdapter(baseUrl) {
  const dist = process.env.BENCH_NODE_DIST;
  if (!dist) throw new Error('BENCH_NODE_DIST is required for BENCH_TARGET=node');
  const child = spawn(
    process.execPath,
    [
      '--input-type=module',
      '-e',
      `
    import { createRequire } from 'node:module';
    const require = createRequire(import.meta.url);
    const { createServer, findAvailablePort } = require(process.env.BENCH_NODE_ENTRY);
    console.log = () => {};
    const server = createServer({ baseUrl: process.env.BENCH_BASE_URL, apiKey: 'benchmark' });
    const url = await server.start(await findAvailablePort(0));
    process.stdout.write(JSON.stringify({url}) + '\\n');
    process.stdin.resume();
    process.stdin.on('end', async () => { await server.stop(); process.exit(0); });
  `,
    ],
    {
      stdio: ['pipe', 'pipe', 'ignore'],
      env: {
        ...process.env,
        BENCH_NODE_ENTRY: path.resolve(dist, 'server', 'index.js'),
        BENCH_BASE_URL: baseUrl,
      },
    }
  );
  const url = await new Promise((resolveReady, reject) => {
    child.once('error', reject);
    child.once('exit', (code) => reject(new Error(`Node adapter exited before ready: ${code}`)));
    child.stdout.once('data', (data) => resolveReady(JSON.parse(data.toString().trim()).url));
  });
  return {
    url,
    pid: child.pid,
    stop: async () => {
      const exited = new Promise((resolveExit) => child.once('exit', resolveExit));
      child.stdin.end();
      await exited;
    },
  };
}

function listen(server) {
  return new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolveListen);
  });
}

function close(server) {
  return new Promise((resolveClose, reject) => {
    server.close((error) => (error ? reject(error) : resolveClose()));
  });
}
