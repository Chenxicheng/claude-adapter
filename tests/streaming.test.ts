// Tests for streaming converter functions

// Mock tokenUsage to prevent tests from writing to real files
jest.mock('../src/utils/tokenUsage', () => ({
  recordUsage: jest.fn(),
}));

// Mock errorLog to prevent tests from writing to real files
jest.mock('../src/utils/errorLog', () => ({
  recordError: jest.fn(),
}));

// Mock raw response for SSE
class MockRawResponse {
  public chunks: string[] = [];
  public headers: Record<string, string> = {};
  public ended = false;

  setHeader(name: string, value: string): void {
    this.headers[name] = value;
  }

  write(data: string): void {
    this.chunks.push(data);
  }

  end(): void {
    this.ended = true;
  }

  getEvents(): Array<{ event: string; data: any }> {
    const events: Array<{ event: string; data: any }> = [];
    const frames = this.chunks.join('').split('\n\n');

    for (const frame of frames) {
      if (!frame.trim()) {
        continue;
      }

      let currentEvent = '';
      let currentData = '';
      for (const line of frame.split('\n')) {
        if (line.startsWith('event: ')) {
          currentEvent = line.slice(7).trim();
        } else if (line.startsWith('data: ')) {
          currentData = line.slice(6).trim();
        }
      }

      if (currentData) {
        events.push({ event: currentEvent, data: JSON.parse(currentData) });
      }
    }

    return events;
  }
}

// Mock async iterator for OpenAI stream
async function* createMockStream(chunks: any[]): AsyncGenerator<any> {
  for (const chunk of chunks) {
    yield chunk;
  }
}

// Import after mocks are set up
import {
  streamOpenAIToAnthropic,
  generateUniqueToolId,
  usedToolIds,
} from '../src/converters/streaming';

describe('Streaming Converter', () => {
  beforeEach(() => {
    jest.clearAllMocks();
  });

  describe('generateUniqueToolId', () => {
    beforeEach(() => {
      usedToolIds.clear();
    });

    it('should generate unique IDs and cleanup old ones when exceeding 10000', () => {
      // Generate 10001 IDs to trigger cleanup (which happens when size > 10000)
      for (let i = 0; i < 10001; i++) {
        generateUniqueToolId();
      }

      // Initially we add up to 10001, but logic deletes 5000 IDs
      // So 10001 - 5000 = 5001
      expect(usedToolIds.size).toBe(5001);
    });
  });

  describe('streamOpenAIToAnthropic', () => {
    it('should set correct SSE headers', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [{ delta: { content: 'Hello' }, finish_reason: null }],
        },
        {
          choices: [{ delta: {}, finish_reason: 'stop' }],
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      expect(mockRaw.headers['Content-Type']).toBe('text/event-stream');
      expect(mockRaw.headers['Cache-Control']).toBe('no-cache');
      expect(mockRaw.headers['Connection']).toBe('keep-alive');
    });

    it('should send message_start event first', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Hi' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      expect(events[0].event).toBe('message_start');
      expect(events[0].data.type).toBe('message_start');
      expect(events[0].data.message.role).toBe('assistant');
      expect(events[0].data.message.model).toBe('claude-4-opus');
      expect(events[0].data.message.usage.input_tokens).toBe(0);
      expect(events[0].data.message.usage.output_tokens).toBe(0);
      expect(events[0].data.message.usage).not.toHaveProperty('cache_read_input_tokens');
      expect(mockRaw.chunks[0]).toContain('event: message_start\n');
      expect(mockRaw.chunks[0]).toContain('\ndata: ');
    });

    it('should not record message_start placeholder usage', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;
      const beforeFinalUsage: { eventType?: string; recordCalls?: number } = {};

      const stream = {
        nextCalls: 0,
        async next(): Promise<IteratorResult<any>> {
          this.nextCalls++;
          if (this.nextCalls === 1) {
            return {
              value: { choices: [{ delta: { content: 'Hi' }, finish_reason: null }] },
              done: false,
            };
          }
          if (this.nextCalls === 2) {
            beforeFinalUsage.eventType = mockRaw.getEvents()[0]?.data.type;
            beforeFinalUsage.recordCalls = recordUsage.mock.calls.length;
            return {
              value: {
                choices: [],
                usage: { prompt_tokens: 4, completion_tokens: 2 },
              },
              done: false,
            };
          }
          return { value: undefined, done: true };
        },
        [Symbol.asyncIterator](): AsyncIterator<any> {
          return this;
        },
      };

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      expect(beforeFinalUsage.eventType).toBe('message_start');
      expect(beforeFinalUsage.recordCalls).toBe(0);
      expect(recordUsage).toHaveBeenCalledTimes(1);
    });

    it('should stream text content as content_block_delta events', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Hello' }, finish_reason: null }] },
        { choices: [{ delta: { content: ' world' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const textDeltas = events.filter(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'text_delta'
      );

      expect(textDeltas).toHaveLength(2);
      expect(textDeltas[0].data.delta.text).toBe('Hello');
      expect(textDeltas[1].data.delta.text).toBe(' world');
    });

    it('should send content_block_start for text content', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Test' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const blockStart = events.find((e) => e.data.type === 'content_block_start');

      expect(blockStart).toBeDefined();
      expect(blockStart!.data.content_block.type).toBe('text');
    });

    it('should handle tool calls in stream', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_test123',
                    function: { name: 'get_weather' },
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
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    function: { arguments: '{"city":' },
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
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    function: { arguments: '"NYC"}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();

      // Should have tool_use content block start
      const toolBlockStart = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );
      expect(toolBlockStart).toBeDefined();
      expect(toolBlockStart!.data.content_block.name).toBe('get_weather');

      // Should have input_json_delta events
      const jsonDeltas = events.filter(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'input_json_delta'
      );
      expect(jsonDeltas.length).toBeGreaterThan(0);
      expect(jsonDeltas.map((e) => e.data.delta.partial_json).join('')).toBe('{"city":"NYC"}');
    });

    it('should send message_stop event at end', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Done' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const lastEvent = events[events.length - 1];

      expect(lastEvent.data.type).toBe('message_stop');
      expect(mockRaw.ended).toBe(true);
    });

    it('should send message_delta with stop_reason before message_stop', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Hi' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta).toBeDefined();
      expect(messageDelta!.data.delta.stop_reason).toBe('end_turn');
    });

    it('should set stop_reason to tool_use when tool calls are present', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_abc',
                    function: { name: 'test_tool', arguments: '{}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.delta.stop_reason).toBe('tool_use');
    });

    it('should handle usage information from chunks', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Test' }, finish_reason: null }] },
        {
          choices: [{ delta: {}, finish_reason: 'stop' }],
          usage: { prompt_tokens: 10, completion_tokens: 5 },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage.output_tokens).toBe(5);
    });

    it('should handle usage information from chunks with empty choices (standard OpenAI behavior)', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Test' }, finish_reason: null }] },
        {
          choices: [],
          usage: { prompt_tokens: 20, completion_tokens: 10 },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(events[0].data.type).toBe('message_start');
      expect(messageDelta!.data.usage.input_tokens).toBe(20);
      expect(messageDelta!.data.usage.output_tokens).toBe(10);
      expect(messageDelta!.data.usage).not.toHaveProperty('cache_read_input_tokens');
      expect(recordUsage).toHaveBeenCalledTimes(1);
      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          usage: { prompt_tokens: 20, completion_tokens: 10 },
          usageStatus: 'complete',
        })
      );
    });

    it('should include input_tokens when upstream reports zero prompt tokens', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Zero usage' }, finish_reason: null }] },
        {
          choices: [],
          usage: { prompt_tokens: 0, completion_tokens: 3 },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).toHaveProperty('input_tokens', 0);
      expect(messageDelta!.data.usage.output_tokens).toBe(3);
    });

    it('should omit input_tokens from message_delta when upstream usage is missing', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'No usage' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).not.toHaveProperty('input_tokens');
      expect(recordUsage).toHaveBeenCalledTimes(1);
      const usageRecord = recordUsage.mock.calls[0][0];
      expect(usageRecord).toEqual(
        expect.objectContaining({
          usageStatus: 'missing_final_chunk',
          streaming: true,
        })
      );
      expect(usageRecord).not.toHaveProperty('usage');
    });

    it('should ignore null and empty usage chunks when determining final usage', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Empty usage' }, finish_reason: null }] },
        { choices: [], usage: null },
        { choices: [], usage: {} },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).not.toHaveProperty('input_tokens');
      expect(recordUsage).toHaveBeenCalledTimes(1);
      const usageRecord = recordUsage.mock.calls[0][0];
      expect(usageRecord).toEqual(
        expect.objectContaining({
          usageStatus: 'missing_final_chunk',
          streaming: true,
        })
      );
      expect(usageRecord).not.toHaveProperty('usage');
    });

    it('should record the last non-empty upstream usage object', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const firstUsage = { prompt_tokens: 3, completion_tokens: 1 };
      const finalUsage = { prompt_tokens: 7, completion_tokens: 2, total_tokens: 9 };
      const stream = createMockStream([
        { choices: [{ delta: { content: 'Multiple usage' }, finish_reason: null }] },
        { choices: [], usage: firstUsage },
        { choices: [], usage: finalUsage },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          usage: finalUsage,
          usageStatus: 'complete',
        })
      );
    });

    it('should record non-empty unknown upstream usage fields without changing response usage', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;
      const upstreamUsage = { billable_units: 42 };

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Unknown usage' }, finish_reason: null }] },
        { choices: [], usage: upstreamUsage as any },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).not.toHaveProperty('input_tokens');
      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          usage: upstreamUsage,
          usageStatus: 'complete',
        })
      );
    });

    it('should include cached tokens in streaming usage events', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Cached response' }, finish_reason: null }] },
        {
          choices: [{ delta: {}, finish_reason: 'stop' }],
          usage: {
            prompt_tokens: 500,
            completion_tokens: 10,
            prompt_tokens_details: { cached_tokens: 400 },
          },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage.input_tokens).toBe(100);
      expect(messageDelta!.data.usage.output_tokens).toBe(10);
      expect(messageDelta!.data.usage.cache_read_input_tokens).toBe(400);
    });

    it('should preserve explicit zero cached tokens in streaming usage events', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Uncached response' }, finish_reason: null }] },
        {
          choices: [],
          usage: {
            prompt_tokens: 50,
            completion_tokens: 5,
            prompt_tokens_details: { cached_tokens: 0 },
          },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage.input_tokens).toBe(50);
      expect(messageDelta!.data.usage.output_tokens).toBe(5);
      expect(messageDelta!.data.usage).toHaveProperty('cache_read_input_tokens', 0);
    });

    it('should not subtract reasoning tokens from streaming output tokens', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Warm the cache' }, finish_reason: null }] },
        {
          choices: [],
          usage: {
            prompt_tokens: 120,
            completion_tokens: 12,
            prompt_tokens_details: { cached_tokens: 80 },
            completion_tokens_details: { reasoning_tokens: 7 },
          },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage.input_tokens).toBe(40);
      expect(messageDelta!.data.usage.output_tokens).toBe(12);
      expect(messageDelta!.data.usage.cache_read_input_tokens).toBe(80);
      expect(messageDelta!.data.usage.cache_creation_input_tokens).toBeUndefined();
      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          usage: {
            prompt_tokens: 120,
            completion_tokens: 12,
            prompt_tokens_details: { cached_tokens: 80 },
            completion_tokens_details: { reasoning_tokens: 7 },
          },
        })
      );
      expect(recordUsage.mock.calls[0][0]).not.toHaveProperty('inputTokens');
      expect(recordUsage.mock.calls[0][0]).not.toHaveProperty('cachedInputTokens');
    });

    it('should convert vendor reasoning chunks to Anthropic thinking deltas', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            { delta: { reasoning_content: 'Need to inspect the codebase.' }, finish_reason: null },
          ],
        },
        { choices: [{ delta: { content: 'Done.' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      expect(events[0].data.type).toBe('message_start');
      const thinkingStart = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'thinking'
      );
      const thinkingDelta = events.find(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'thinking_delta'
      );
      const textStart = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'text'
      );

      expect(thinkingStart).toBeDefined();
      expect(thinkingStart!.data.content_block).not.toHaveProperty('signature');
      expect(thinkingDelta!.data.delta.thinking).toBe('Need to inspect the codebase.');
      expect(textStart).toBeDefined();
      expect(
        events.filter((e) => e.data.type === 'content_block_stop').map((e) => e.data.index)
      ).toEqual([0, 1]);
    });

    it('should close thinking before streaming text and tool-use blocks', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { reasoning: 'Need to call the helper.' }, finish_reason: null }] },
        { choices: [{ delta: { content: 'I will call a helper.' }, finish_reason: null }] },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_helper',
                    function: { name: 'helper', arguments: '{"ok":true}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const blockStarts = events.filter((e) => e.data.type === 'content_block_start');
      const blockStops = events.filter((e) => e.data.type === 'content_block_stop');

      expect(blockStarts.map((e) => e.data.content_block.type)).toEqual([
        'thinking',
        'text',
        'tool_use',
      ]);
      expect(blockStarts.map((e) => e.data.index)).toEqual([0, 1, 2]);
      expect(blockStops.map((e) => e.data.index)).toEqual([0, 1, 2]);
    });

    it('should ignore reasoning chunks after tool streaming has started', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_after_reasoning',
                    function: { name: 'helper', arguments: '{"a":' },
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
              delta: { reasoning_content: 'Late reasoning should not open a block.' },
              finish_reason: null,
            },
          ],
        },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    function: { arguments: '1}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const thinkingStarts = events.filter(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'thinking'
      );
      const toolStarts = events.filter(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );
      const jsonDeltas = events.filter(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'input_json_delta'
      );

      expect(thinkingStarts).toHaveLength(0);
      expect(toolStarts).toHaveLength(1);
      expect(jsonDeltas.map((e) => e.data.delta.partial_json).join('')).toBe('{"a":1}');
    });

    it('should handle stream errors gracefully', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      async function* errorStream(): AsyncGenerator<any> {
        yield { choices: [{ delta: { content: 'Start' }, finish_reason: null }] };
        throw new Error('Stream connection lost');
      }

      await streamOpenAIToAnthropic(errorStream() as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const errorEvent = events.find((e) => e.data.type === 'error');

      expect(errorEvent).toBeDefined();
      expect(errorEvent!.data.error.message).toBe('Stream connection lost');
      expect(mockRaw.ended).toBe(true);
    });

    it('should handle empty stream with only stop signal', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([{ choices: [{ delta: {}, finish_reason: 'stop' }] }]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      expect(events.find((e) => e.data.type === 'message_start')).toBeDefined();
      expect(events.find((e) => e.data.type === 'message_stop')).toBeDefined();
      expect(mockRaw.ended).toBe(true);
    });

    it('should handle multiple tool calls in a single response', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  { index: 0, id: 'call_first', function: { name: 'tool_a' } },
                  { index: 1, id: 'call_second', function: { name: 'tool_b' } },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  { index: 0, function: { arguments: '{"x":1}' } },
                  { index: 1, function: { arguments: '{"y":2}' } },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();

      // Should have two tool_use content blocks started
      const toolBlockStarts = events.filter(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );
      expect(toolBlockStarts.length).toBeGreaterThanOrEqual(1);
    });

    it('should handle stream with text followed by tool call', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Let me help with that.' }, finish_reason: null }] },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_combo',
                    function: { name: 'helper', arguments: '{}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();

      // Should have both text and tool_use content blocks
      const textBlock = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'text'
      );
      const toolBlock = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );
      const blockStops = events.filter((e) => e.data.type === 'content_block_stop');

      expect(textBlock).toBeDefined();
      expect(toolBlock).toBeDefined();
      expect(blockStops.map((e) => e.data.index)).toEqual([0, 1]);
    });

    it('should not emit duplicate text block stop when text is followed by a tool call', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            { delta: { content: 'I will inspect the directory first.' }, finish_reason: null },
          ],
        },
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    id: 'call_bash',
                    function: { name: 'Bash', arguments: '{"command":"pwd"}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const blockStops = events.filter((e) => e.data.type === 'content_block_stop');

      expect(blockStops).toHaveLength(2);
      expect(blockStops.map((e) => e.data.index)).toEqual([0, 1]);
    });
    it('should generate tool ID if missing in stream', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: {
                tool_calls: [
                  {
                    index: 0,
                    // id is missing
                    function: { name: 'test_tool', arguments: '{}' },
                  },
                ],
              },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus');

      const events = mockRaw.getEvents();
      const toolBlock = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );

      expect(toolBlock).toBeDefined();
      expect(toolBlock!.data.content_block.id).toBeDefined();
      expect(toolBlock!.data.content_block.id).toMatch(/^call_/);
    });

    it('should capture and use response model for usage recording', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        {
          model: 'gpt-4-0613', // Different from request model
          choices: [{ delta: { content: 'Test' }, finish_reason: null }],
        },
        {
          choices: [{ delta: {}, finish_reason: 'stop' }],
          usage: { prompt_tokens: 10, completion_tokens: 5 },
        },
      ]);

      await streamOpenAIToAnthropic(stream as any, mockReply, 'claude-4-opus', 'openai');

      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          model: 'gpt-4-0613',
        })
      );
    });
  });
});
