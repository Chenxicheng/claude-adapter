#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

const target = process.env.BENCH_TARGET ?? 'rust';
const repetitions = Number(process.env.BENCH_REPETITIONS ?? 5);
const upstreamDelayMs = Number(process.env.BENCH_UPSTREAM_DELAY_MS ?? 0);
const concurrencies = [1, 10, 50, 100];
const payload = {
  model: 'benchmark-model',
  max_tokens: 128,
  messages: [{ role: 'user', content: 'Reply with benchmark.' }],
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
        errors: samples.reduce((total, sample) => total + sample.errors, 0),
      };
    })
  );
}
process.exit(0);

async function runRequests(url, total, concurrency, streaming) {
  const latencies = [];
  const firstContent = [];
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
          const reader = response.body.getReader();
          const decoder = new TextDecoder();
          let text = '';
          let found = false;
          while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            text += decoder.decode(value, { stream: true });
            if (!found && text.includes('text_delta')) {
              firstContent.push(performance.now() - requestStarted);
              found = true;
            }
          }
        } else {
          await response.arrayBuffer();
          firstContent.push(performance.now() - requestStarted);
        }
        latencies.push(performance.now() - requestStarted);
      } catch (error) {
        errors++;
        firstError ??= error.message;
      }
    }
  });
  await Promise.all(workers);
  const durationMs = performance.now() - started;
  latencies.sort((left, right) => left - right);
  firstContent.sort((left, right) => left - right);
  return {
    requests: total,
    errors,
    firstError,
    durationMs,
    throughput: total / (durationMs / 1000),
    p50Ms: percentile(latencies, 0.5),
    p95Ms: percentile(latencies, 0.95),
    p99Ms: percentile(latencies, 0.99),
    firstContentP50Ms: percentile(firstContent, 0.5),
    harnessRssBytes: process.memoryUsage().rss,
  };
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
    firstChatHeaders['user-agent'] !== 'OpenAI/JS 4.76.0'
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
  if (!toolResponse.ok || toolBody.content.map((part) => part.type).join(',') !== 'tool_use') {
    throw new Error('Reasoning/tool parity preflight failed');
  }
  const toolRequest = mock.requests.at(-1);
  if (!toolRequest.tools?.length)
    throw new Error('Tool request conversion preflight failed');
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
  const reader = response.body.getReader();
  await reader.read();
  const drain = (async () => {
    while (!(await reader.read()).done) {}
  })();
  await adapter.stop();
  await drain;
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
      apiKey: 'benchmark',
      models: { opus: 'benchmark-model', sonnet: 'benchmark-model', haiku: 'benchmark-model' },
      upstreamHeaders: {
        Accept: 'text/plain',
        Authorization: 'Bearer wrong',
        'Content-Type': 'text/plain',
        'User-Agent': 'wrong',
      },
    })
  );
  const child = spawn(
    path.resolve('native/target/release/claude-adapter-native'),
    ['--config', config, '--port', '0'],
    { stdio: ['ignore', 'pipe', process.env.BENCH_DEBUG ? 'inherit' : 'ignore'] }
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
    stop: async () => {
      if (stopped) return;
      stopped = true;
      child.kill('SIGTERM');
      await new Promise((resolveExit) => child.once('exit', resolveExit));
      await rm(directory, { recursive: true, force: true });
    },
  };
}

async function startNodeAdapter(baseUrl) {
  const dist = process.env.BENCH_NODE_DIST;
  if (!dist) throw new Error('BENCH_NODE_DIST is required for BENCH_TARGET=node');
  const require = createRequire(import.meta.url);
  const { createServer, findAvailablePort } = require(path.join(dist, 'server', 'index.js'));
  const originalLog = console.log;
  console.log = () => {};
  const server = createServer({
    baseUrl,
    apiKey: 'benchmark',
    models: { opus: 'benchmark-model', sonnet: 'benchmark-model', haiku: 'benchmark-model' },
  });
  try {
    const port = await findAvailablePort(0);
    const url = await server.start(port);
    return {
      url,
      stop: async () => {
        await server.stop();
        console.log = originalLog;
      },
    };
  } catch (error) {
    console.log = originalLog;
    throw error;
  }
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
