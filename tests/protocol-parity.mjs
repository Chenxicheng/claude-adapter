#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import ts from 'typescript';
import Anthropic from '@anthropic-ai/sdk';
import { readAnthropicStream } from '../bench/stream-validation.mjs';

// Read the immutable oracle from Git; never import the deleted TS server at runtime.
const modules = new Map();
function mainModule(file) {
  if (modules.has(file)) return modules.get(file).exports;
  const module = { exports: {} };
  modules.set(file, module);
  const source = execFileSync('git', ['show', `8a19608:${file}`], { encoding: 'utf8' });
  const javascript = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  new Function('require', 'module', 'exports', javascript)(
    (name) => {
      if (name.endsWith('/tokenUsage')) return { recordUsage() {} };
      if (name.endsWith('/errorLog')) return { recordError() {} };
      return mainModule(
        path.posix.normalize(path.posix.join(path.posix.dirname(file), `${name}.ts`))
      );
    },
    module,
    module.exports
  );
  return module.exports;
}
const oracle = mainModule('src/converters/streaming.ts');
const responseOracle = mainModule('src/converters/response.ts');
const chunk = (delta, finish_reason = null) => ({ choices: [{ delta, finish_reason }] });
const tool = (index, name, args) => ({
  index,
  id: `call_${index}`,
  type: 'function',
  function: { name, arguments: args },
});
const usage = { choices: [], usage: { prompt_tokens: 5, completion_tokens: 3 } };
const cases = {
  text: { chunks: [chunk({ content: 'answer' }, 'stop'), usage], parity: true },
  thinking: {
    chunks: [chunk({ reasoning_content: '思考🙂' }), chunk({ content: 'answer' }, 'stop'), usage],
    parity: true,
  },
  fallback: {
    chunks: [chunk({ reasoning_content: null, reasoning: 'think' }, 'stop')],
    parity: true,
  },
  reasoningOnly: { chunks: [chunk({ reasoning_content: 'think' }, 'stop')], parity: true },
  tool: {
    chunks: [
      chunk({ reasoning: 'think' }),
      chunk({ tool_calls: [tool(0, 'lookup', '{"id":')] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '1}' } }] }, 'tool_calls'),
      usage,
    ],
    parity: true,
  },
  repeatedToolName: {
    chunks: [
      chunk({ tool_calls: [tool(0, 'lookup', '{')] }),
      chunk(
        { tool_calls: [{ index: 0, function: { name: 'lookup', arguments: '"id":1}' } }] },
        'tool_calls'
      ),
    ],
    parity: true,
  },
  parallel: {
    chunks: [
      chunk({ tool_calls: [tool(0, 'lookup', '{'), tool(1, 'other', '{')] }),
      chunk(
        {
          tool_calls: [
            { index: 1, function: { arguments: '"b":2}' } },
            { index: 0, function: { arguments: '"a":1}' } },
          ],
        },
        'tool_calls'
      ),
      usage,
    ],
    parity: true,
  },
  toolThenText: {
    chunks: [
      chunk({ tool_calls: [tool(3, 'lookup', '{}')] }),
      chunk({ content: 'after tool' }, 'tool_calls'),
    ],
    text: 'after tool',
  },
  fragmentedName: {
    chunks: [
      chunk({ tool_calls: [tool(0, 'look', '{"id":')] }),
      chunk(
        { tool_calls: [{ index: 0, function: { name: 'up', arguments: '1}' } }] },
        'tool_calls'
      ),
    ],
    name: 'lookup',
  },
  ambiguousName: {
    chunks: [chunk({ tool_calls: [tool(0, 'look', '')] }), chunk({}, 'tool_calls')],
    name: 'look',
  },
  explicitDone: { chunks: [chunk({ content: 'complete' })], text: 'complete' },
  eofAfterFinish: {
    chunks: [chunk({ content: 'complete' }, 'stop')],
    done: false,
    text: 'complete',
  },
  emptyChoice: { chunks: [{ choices: [] }, chunk({ content: 'answer' }, 'stop')], parity: true },
  postFinishEmpty: {
    chunks: [chunk({ content: 'answer' }, 'stop'), chunk({}, 'stop'), usage],
    parity: true,
  },
  postFinishContent: {
    chunks: [chunk({ content: 'answer' }, 'stop'), chunk({ content: 'late' })],
    error: true,
  },
  postFinishConflict: {
    chunks: [chunk({ content: 'answer' }, 'stop'), chunk({}, 'length')],
    error: true,
  },
  changedToolName: {
    chunks: [
      chunk({ tool_calls: [tool(0, 'lookup', '{')] }),
      chunk({ tool_calls: [{ index: 0, function: { name: 'other', arguments: '}' } }] }),
    ],
    error: true,
  },
  length: { chunks: [chunk({ content: 'partial' }, 'length')], reason: 'max_tokens' },
  truncated: { chunks: [chunk({ content: 'partial' })], done: false, error: true },
  badArguments: {
    chunks: [chunk({ tool_calls: [tool(0, 'lookup', '{')] }, 'tool_calls')],
    error: true,
  },
  undeclared: {
    chunks: [chunk({ tool_calls: [tool(0, 'missing', '{}')] }, 'tool_calls')],
    error: true,
  },
  upstreamError: {
    chunks: [chunk({ content: 'partial' }), { error: { message: 'mock failure' } }],
    error: true,
  },
};
const fixtures = JSON.parse(await readFile('bench/fixtures/response-conversion.json', 'utf8'));
const upstream = http.createServer(async (request, response) => {
  const buffers = [];
  for await (const buffer of request) buffers.push(buffer);
  const body = JSON.parse(Buffer.concat(buffers));
  if (!body.stream) {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify(fixtures[Number(body.model)].input));
    return;
  }
  const scenario = cases[body.model];
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const separator = body.model === 'thinking' ? '\r' : '\r\n';
  const wire =
    scenario.chunks
      .map((item) => `data: ${JSON.stringify(item)}${separator}${separator}`)
      .join('') + (scenario.done === false ? '' : `data: [DONE]${separator}${separator}`);
  if (body.model === 'thinking') {
    for (const byte of Buffer.from(wire)) response.write(Buffer.from([byte]));
    response.end();
  } else response.end(wire);
});
await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
const directory = await mkdtemp(path.join(os.tmpdir(), 'adapter-protocol-'));
const config = path.join(directory, 'config.json');
await writeFile(
  config,
  JSON.stringify({
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    apiKey: 'local-mock',
  })
);
const binary = path.resolve(
  process.env.ADAPTER_BINARY ??
    `native/target/release/claude-adapter-native${process.platform === 'win32' ? '.exe' : ''}`
);
const child = spawn(binary, ['--config', config, '--port', '0'], {
  stdio: ['pipe', 'pipe', 'ignore'],
  env: { ...process.env, CLAUDE_ADAPTER_STDIN_SHUTDOWN: '1' },
});
let completed = 0;
try {
  const url = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Adapter startup timed out')), 10000);
    child.once('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`Adapter exited: ${code}`));
    });
    let buffer = '';
    child.stdout.on('data', (data) => {
      buffer += data;
      const line = buffer.split('\n').find((value) => value.startsWith('CLAUDE_ADAPTER_READY='));
      if (line) {
        clearTimeout(timer);
        resolve(JSON.parse(line.split('=')[1]).url);
      }
    });
  });
  const client = new Anthropic({
    baseURL: url,
    apiKey: 'local-mock',
    maxRetries: 0,
    timeout: 10000,
  });
  const request = (model) => ({
    model,
    max_tokens: 100,
    stream: true,
    messages: [{ role: 'user', content: 'test' }],
    tools: ['lookup', 'other', 'look']
      .filter((name) => name !== 'look' || ['ambiguousName', 'fragmentedName'].includes(model))
      .map((name) => ({ name, input_schema: { type: 'object' } })),
  });
  for (const [name, scenario] of Object.entries(cases)) {
    const response = await fetch(`${url}/v1/messages`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request(name)),
      signal: AbortSignal.timeout(10000),
    });
    if (scenario.error) {
      const text = await response.text();
      assert.ok(text.includes('event: error'), name);
      assert.ok(!text.includes('event: message_stop'), name);
      assert.ok(!text.includes('event: message_delta'), name);
    } else {
      const actual = [];
      const result = await readAnthropicStream(response, (event) => actual.push(event));
      if (scenario.parity) {
        oracle.usedToolIds.clear();
        const expected = [];
        await oracle.streamOpenAIToAnthropic(
          (async function* () {
            yield* scenario.chunks;
          })(),
          {
            raw: {
              setHeader() {},
              write(data) {
                expected.push(JSON.parse(data.split('\ndata: ')[1]));
              },
              end() {},
            },
          },
          name
        );
        actual[0].message.id = expected[0].message.id = 'normalized';
        assert.deepEqual(actual, expected, `${name}: main event parity`);
      }
      if (scenario.text)
        assert.equal(result.blocks.map((block) => block.text ?? '').join(''), scenario.text);
      if (scenario.name)
        assert.equal(result.blocks.find((block) => block.type === 'tool_use').name, scenario.name);
      if (scenario.reason) assert.equal(result.stopReason, scenario.reason);
      // Check the SDK snapshot, not its latest-block convenience callbacks.
      const snapshot = await client.messages.stream(request(name)).finalMessage();
      assert.deepEqual(
        snapshot.content,
        result.blocks.map(({ arguments: _args, closed: _closed, ...block }) => block),
        `${name}: SDK snapshot`
      );
    }
    completed++;
  }
  for (const [index, fixture] of fixtures.entries()) {
    const actual = await client.messages.create({ ...request(String(index)), stream: false });
    assert.deepEqual(
      actual,
      responseOracle.convertResponseToAnthropic(fixture.input, String(index))
    );
    completed++;
  }
  console.log(JSON.stringify({ passed: completed, oracle: '8a19608', sdk: '0.71.2', errors: 0 }));
} finally {
  if (child.exitCode === null) {
    const exited = new Promise((resolve) => child.once('exit', resolve));
    child.stdin.end();
    const timer = setTimeout(() => child.kill('SIGKILL'), 10000);
    await exited;
    clearTimeout(timer);
  }
  await new Promise((resolve) => upstream.close(resolve));
  await rm(directory, { recursive: true, force: true });
}
