import assert from 'node:assert/strict';
import { Stream } from '@anthropic-ai/sdk/streaming';

// Shared by protocol acceptance and benchmarks: HTTP 200 alone is not success.
export async function readAnthropicStream(response, onEvent = () => {}) {
  assert.equal(response.status, 200);
  const blocks = new Map();
  let started = false;
  let stopped = false;
  let finalUsage;
  let stopReason;
  let nextIndex = 0;
  for await (const event of Stream.fromSSEResponse(response, new AbortController())) {
    assert.equal(stopped, false, 'event after message_stop');
    onEvent(event);
    switch (event.type) {
      case 'message_start':
        assert.equal(started, false);
        started = true;
        break;
      case 'content_block_start':
        assert.ok(started && !finalUsage);
        assert.equal(event.index, nextIndex++);
        blocks.set(event.index, { ...event.content_block, arguments: '', closed: false });
        break;
      case 'content_block_delta': {
        const block = blocks.get(event.index);
        assert.ok(block && !block.closed && !finalUsage, 'delta targets an open block');
        const fields = {
          text_delta: ['text', 'text'],
          thinking_delta: ['thinking', 'thinking'],
          input_json_delta: ['arguments', 'partial_json'],
        };
        const field = fields[event.delta.type];
        assert.ok(field, `unexpected delta ${event.delta.type}`);
        const expectedType = {
          text_delta: 'text',
          thinking_delta: 'thinking',
          input_json_delta: 'tool_use',
        }[event.delta.type];
        assert.equal(block.type, expectedType);
        block[field[0]] += event.delta[field[1]];
        break;
      }
      case 'content_block_stop': {
        const block = blocks.get(event.index);
        assert.ok(block && !block.closed && !finalUsage, 'stop targets an open block');
        if (block.type === 'tool_use') {
          block.input = JSON.parse(block.arguments || '{}');
          assert.ok(block.input && typeof block.input === 'object' && !Array.isArray(block.input));
        }
        block.closed = true;
        break;
      }
      case 'message_delta':
        assert.ok(started && !finalUsage);
        assert.ok([...blocks.values()].every((block) => block.closed));
        finalUsage = event.usage;
        assert.equal(typeof finalUsage.output_tokens, 'number');
        stopReason = event.delta.stop_reason;
        break;
      case 'message_stop':
        assert.ok(started && finalUsage && stopReason);
        stopped = true;
        break;
      case 'ping':
        break;
      default:
        throw new Error(`Unexpected SSE event: ${event.type}`);
    }
  }
  assert.ok(stopped, 'stream ended without message_stop');
  return { blocks: [...blocks.values()], usage: finalUsage, stopReason };
}
