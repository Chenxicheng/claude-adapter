// Tests for XML Streaming Converter

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
    let currentEvent = '';

    for (const chunk of this.chunks) {
      if (chunk.startsWith('event: ')) {
        currentEvent = chunk.slice(7).trim();
      } else if (chunk.startsWith('data: ')) {
        const data = JSON.parse(chunk.slice(6).trim());
        events.push({ event: currentEvent, data });
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
import { streamXmlOpenAIToAnthropic } from '../src/converters/xmlStreaming';

describe('XML Streaming Converter', () => {
  beforeEach(() => {
    jest.clearAllMocks();
  });

  describe('streamXmlOpenAIToAnthropic', () => {
    it('should set correct SSE headers', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Hello' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      expect(mockRaw.headers['Content-Type']).toBe('text/event-stream');
      expect(mockRaw.headers['Cache-Control']).toBe('no-cache');
    });

    it('should stream plain text without tool calls', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Hello world' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();

      expect(events[0].data.type).toBe('message_start');
      expect(events[0].data.message.usage.input_tokens).toBe(0);
      expect(events[0].data.message.usage.output_tokens).toBe(0);
      expect(events[0].data.message.usage).not.toHaveProperty('cache_read_input_tokens');

      const textDelta = events.find(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'text_delta'
      );
      expect(textDelta).toBeDefined();
      expect(textDelta!.data.delta.text).toBe('Hello world');
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
              value: { choices: [{ delta: { content: 'Hello' }, finish_reason: null }] },
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

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      expect(beforeFinalUsage.eventType).toBe('message_start');
      expect(beforeFinalUsage.recordCalls).toBe(0);
      expect(recordUsage).toHaveBeenCalledTimes(1);
    });

    it('should detect XML tool call and emit tool_use events', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: { content: '<tool_code name="get_weather">{"city": "NYC"}</tool_code>' },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();

      // Should have tool_use content block start
      const toolBlockStart = events.find(
        (e) => e.data.type === 'content_block_start' && e.data.content_block?.type === 'tool_use'
      );
      expect(toolBlockStart).toBeDefined();
      expect(toolBlockStart!.data.content_block.name).toBe('get_weather');

      // Should have input_json_delta
      const jsonDelta = events.find(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'input_json_delta'
      );
      expect(jsonDelta).toBeDefined();
    });

    it('should handle text before tool call', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        {
          choices: [
            {
              delta: { content: 'Let me help. <tool_code name="helper">{"a":1}</tool_code>' },
              finish_reason: null,
            },
          ],
        },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();

      // Should have text content (trimmed by new implementation)
      const textDelta = events.find(
        (e) => e.data.type === 'content_block_delta' && e.data.delta?.type === 'text_delta'
      );
      expect(textDelta?.data.delta.text).toBe('Let me help.');

      // Should also have tool_use
      const toolBlock = events.find((e) => e.data.content_block?.type === 'tool_use');
      expect(toolBlock).toBeDefined();
    });

    it('should handle streaming tool call across multiple chunks', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: '<tool_code name="test">' }, finish_reason: null }] },
        { choices: [{ delta: { content: '{"key": ' }, finish_reason: null }] },
        { choices: [{ delta: { content: '"value"}' }, finish_reason: null }] },
        { choices: [{ delta: { content: '</tool_code>' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();

      // Should have tool_use block
      const toolBlock = events.find((e) => e.data.content_block?.type === 'tool_use');
      expect(toolBlock).toBeDefined();
      expect(toolBlock!.data.content_block.name).toBe('test');

      // Should have multiple input_json_delta events
      const jsonDeltas = events.filter((e) => e.data.delta?.type === 'input_json_delta');
      expect(jsonDeltas.length).toBeGreaterThan(0);
    });

    it('should send message_stop at end', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Done' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();
      const lastEvent = events[events.length - 1];

      expect(lastEvent.data.type).toBe('message_stop');
      expect(mockRaw.ended).toBe(true);
    });

    it('should handle stream errors', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      async function* errorStream(): AsyncGenerator<any> {
        yield { choices: [{ delta: { content: 'Start' }, finish_reason: null }] };
        throw new Error('Connection lost');
      }

      await streamXmlOpenAIToAnthropic(errorStream() as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();
      const errorEvent = events.find((e) => e.data.type === 'error');

      expect(errorEvent).toBeDefined();
      expect(errorEvent!.data.error.message).toBe('Connection lost');
      expect(mockRaw.ended).toBe(true);
    });

    it('should include final usage in message_delta without delaying message_start', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'Usage check' }, finish_reason: null }] },
        {
          choices: [],
          usage: {
            prompt_tokens: 20,
            completion_tokens: 10,
            prompt_tokens_details: { cached_tokens: 8 },
          },
        },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(events[0].data.type).toBe('message_start');
      expect(messageDelta!.data.usage.input_tokens).toBe(20);
      expect(messageDelta!.data.usage.output_tokens).toBe(10);
      expect(messageDelta!.data.usage.cache_read_input_tokens).toBe(8);
      expect(recordUsage).toHaveBeenCalledTimes(1);
      expect(recordUsage).toHaveBeenCalledWith(
        expect.objectContaining({
          inputTokens: 20,
          outputTokens: 10,
          cachedInputTokens: 8,
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

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).toHaveProperty('input_tokens', 0);
      expect(messageDelta!.data.usage.output_tokens).toBe(3);
      expect(messageDelta!.data.usage).not.toHaveProperty('cache_read_input_tokens');
    });

    it('should preserve explicit zero cached tokens in final usage', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'No cache' }, finish_reason: null }] },
        {
          choices: [],
          usage: {
            prompt_tokens: 12,
            completion_tokens: 4,
            prompt_tokens_details: { cached_tokens: 0 },
          },
        },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

      const events = mockRaw.getEvents();
      const messageDelta = events.find((e) => e.data.type === 'message_delta');

      expect(messageDelta!.data.usage).toHaveProperty('cache_read_input_tokens', 0);
    });

    it('should omit input_tokens from message_delta when upstream usage is missing', async () => {
      const mockRaw = new MockRawResponse();
      const mockReply = { raw: mockRaw } as any;
      const recordUsage = require('../src/utils/tokenUsage').recordUsage;

      const stream = createMockStream([
        { choices: [{ delta: { content: 'No usage' }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: 'stop' }] },
      ]);

      await streamXmlOpenAIToAnthropic(stream as any, mockReply, 'test-model');

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
      expect(usageRecord).not.toHaveProperty('inputTokens');
      expect(usageRecord).not.toHaveProperty('outputTokens');
      expect(usageRecord).not.toHaveProperty('cachedInputTokens');
    });
  });
});
