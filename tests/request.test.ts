// Tests for request converter: Anthropic → OpenAI
import { convertRequestToOpenAI } from '../src/converters/request';
import { AnthropicMessageRequest } from '../src/types/anthropic';
import { isAzureOpenAIEndpoint } from '../src/utils/endpoint';

// Mock update utility
jest.mock('../src/utils/update', () => ({
  getCachedUpdateInfo: jest.fn().mockReturnValue(null), // Default no update
}));

import { getCachedUpdateInfo } from '../src/utils/update';

describe('Request Converter', () => {
  describe('convertRequestToOpenAI', () => {
    it('should convert a simple text message', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [{ role: 'user', content: 'Hello, how are you?' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4.1');

      expect(result.model).toBe('gpt-4.1');
      expect(result.max_tokens).toBe(1024);
      expect(result.max_completion_tokens).toBeUndefined();
      expect(result.messages).toHaveLength(1);
      expect(result.messages[0]).toEqual({
        role: 'user',
        content: 'Hello, how are you?',
      });
    });

    it('should convert max tokens to max completion tokens for Azure OpenAI', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [{ role: 'user', content: 'Hello, how are you?' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.2-codex', true);

      expect(result.max_completion_tokens).toBe(1024);
      expect(result.max_tokens).toBeUndefined();
    });

    it('should convert Azure max tokens of 1 to max completion tokens of 32', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1,
        messages: [{ role: 'user', content: 'Ping' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.2-codex', true);

      expect(result.max_completion_tokens).toBe(32);
      expect(result.max_tokens).toBeUndefined();
    });

    it('should detect Azure OpenAI endpoints', () => {
      expect(isAzureOpenAIEndpoint('https://example.openai.azure.com/openai/v1')).toBe(true);
      expect(isAzureOpenAIEndpoint('https://example.services.ai.azure.com/models')).toBe(true);
      expect(isAzureOpenAIEndpoint('https://api.openai.com/v1')).toBe(false);
      expect(isAzureOpenAIEndpoint('not a url')).toBe(false);
    });

    it('should convert system prompt to system message', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-opus',
        max_tokens: 2048,
        system: 'You are a helpful assistant.',
        messages: [{ role: 'user', content: 'Hi there!' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4.1');

      expect(result.messages).toHaveLength(2);
      expect(result.messages[0]).toEqual({
        role: 'system',
        content: 'You are a helpful assistant.',
      });
    });

    it('should convert system array to concatenated system message', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-opus',
        max_tokens: 2048,
        system: [
          { type: 'text', text: 'You are helpful.' },
          { type: 'text', text: 'Be concise.' },
        ],
        messages: [{ role: 'user', content: 'Hi!' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].role).toBe('system');
      expect(result.messages[0].content).toContain('You are helpful.');
      expect(result.messages[0].content).toContain('Be concise.');
    });

    it('should convert multi-turn conversation', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-haiku',
        max_tokens: 512,
        messages: [
          { role: 'user', content: 'What is 2+2?' },
          { role: 'assistant', content: '2+2 equals 4.' },
          { role: 'user', content: 'And 3+3?' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.2-mini');

      expect(result.messages).toHaveLength(3);
      expect(result.messages[0].role).toBe('user');
      expect(result.messages[1].role).toBe('assistant');
      expect(result.messages[2].role).toBe('user');
    });

    it('should map unknown roles to assistant', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Hello' },
          { role: 'unknown', content: 'Some injected context' },
          { role: 'user', content: 'Follow up' },
        ],
      } as AnthropicMessageRequest;

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(3);
      expect(result.messages[1].role).toBe('assistant');
      expect(result.messages[1].content).toBe('Some injected context');
    });

    it('should skip messages with undefined content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Hello' },
          { role: 'assistant', content: undefined as unknown as string },
          { role: 'user', content: 'Follow up' },
        ],
      } as AnthropicMessageRequest;

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      // Message with undefined content should be skipped
      expect(result.messages).toHaveLength(2);
      expect(result.messages[0].role).toBe('user');
      expect(result.messages[0].content).toBe('Hello');
      expect(result.messages[1].role).toBe('user');
      expect(result.messages[1].content).toBe('Follow up');
    });

    it('should throw an error if all messages have undefined content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: undefined as unknown as string },
          { role: 'assistant', content: null as unknown as string },
        ],
      } as AnthropicMessageRequest;

      expect(() => {
        convertRequestToOpenAI(anthropicRequest, 'gpt-4');
      }).toThrow('No messages after conversion: all input messages had missing content');
    });

    it('should convert content blocks array in user message', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              { type: 'text', text: 'First part.' },
              { type: 'text', text: 'Second part.' },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4.1');

      expect(result.messages).toHaveLength(1);
      expect(result.messages[0].role).toBe('user');
    });

    it('should collapse single text block to string', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [{ type: 'text', text: 'Only one block' }],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].content).toBe('Only one block');
    });

    it('should include optional parameters when provided', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        temperature: 0.7,
        top_p: 0.9,
        stop_sequences: ['END', 'STOP'],
        messages: [{ role: 'user', content: 'Test message' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.2-codex');

      expect(result.temperature).toBe(0.7);
      expect(result.top_p).toBe(0.9);
      expect(result.stop).toEqual(['END', 'STOP']);
    });

    it('should handle stream parameter', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        stream: true,
        messages: [{ role: 'user', content: 'Stream this' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.2-codex');

      expect(result.stream).toBe(true);
    });

    it('should strip a leading anthropic billing header from system content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        system: 'x-anthropic-billing-header: cch=test\nYou are a helpful assistant.',
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4o');

      expect(result.messages[0]).toEqual({
        role: 'system',
        content: 'You are a helpful assistant.',
      });
    });

    it('should use max_completion_tokens for OpenAI o-series and GPT-5 models', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 512,
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const oSeriesResult = convertRequestToOpenAI(anthropicRequest, 'o3');
      const gpt5Result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.5');
      const gpt4Result = convertRequestToOpenAI(anthropicRequest, 'gpt-4o');

      expect(oSeriesResult.max_completion_tokens).toBe(512);
      expect(oSeriesResult.max_tokens).toBeUndefined();
      expect(gpt5Result.max_completion_tokens).toBe(512);
      expect(gpt5Result.max_tokens).toBeUndefined();
      expect(gpt4Result.max_tokens).toBe(512);
      expect(gpt4Result.max_completion_tokens).toBeUndefined();
    });

    it('should map Anthropic thinking to OpenAI reasoning_effort for OpenAI GPT-5 models', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        output_config: { effort: 'max' },
        messages: [{ role: 'user', content: 'Think deeply' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-5.5');

      expect(result.reasoning_effort).toBe('xhigh');
    });

    it('should map Anthropic thinking to GLM thinking and tool streaming', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        stream: true,
        thinking: { type: 'enabled' },
        tools: [
          {
            name: 'Read',
            description: 'Read a file',
            input_schema: {
              type: 'object',
              properties: { path: { type: 'string' } },
            },
          },
        ],
        messages: [{ role: 'user', content: 'Open the file' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'glm-5.1');

      expect(result.thinking).toEqual({ type: 'enabled' });
      expect(result.tool_stream).toBe(true);
      expect(result.reasoning_effort).toBeUndefined();
      expect(result.max_tokens).toBe(1024);
      expect(result.max_completion_tokens).toBeUndefined();
    });

    it('should map Anthropic effort to GLM-5.2 reasoning_effort', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        thinking: { type: 'adaptive' },
        messages: [{ role: 'user', content: 'Plan a migration' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'glm-5.2');

      expect(result.thinking).toEqual({ type: 'enabled' });
      expect(result.reasoning_effort).toBe('max');
    });

    it('should require streaming for Qwen thinking mode', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        thinking: { type: 'enabled' },
        messages: [{ role: 'user', content: 'Think before replying' }],
      };

      expect(() =>
        convertRequestToOpenAI(anthropicRequest, 'qwen3.6-plus')
      ).toThrow('Qwen thinking mode in this adapter requires stream=true');
    });

    it('should enable Qwen thinking mode on streaming requests', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        stream: true,
        thinking: { type: 'adaptive' },
        messages: [{ role: 'user', content: 'Think before replying' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'qwen3.6-plus');

      expect(result.enable_thinking).toBe(true);
      expect(result.reasoning_effort).toBeUndefined();
      expect(result.max_tokens).toBe(1024);
    });

    it('should not leak model-family private fields on generic models', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        stream: true,
        thinking: { type: 'adaptive' },
        messages: [{ role: 'user', content: 'Think before replying' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4o');

      expect(result.thinking).toBeUndefined();
      expect(result.tool_stream).toBeUndefined();
      expect(result.enable_thinking).toBeUndefined();
      expect(result.reasoning_effort).toBeUndefined();
    });

    it('should NOT include user metadata (for provider compatibility)', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        metadata: { user_id: 'user_123' },
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      // user field should NOT be set - some providers (Mistral) reject unknown params
      expect(result.user).toBeUndefined();
    });

    it('should convert tool definitions', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        tools: [
          {
            name: 'get_weather',
            description: 'Get weather',
            input_schema: { type: 'object', properties: {} },
          },
        ],
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.tools).toBeDefined();
      expect(result.tools).toHaveLength(1);
      expect(result.tools![0].function.name).toBe('get_weather');
    });

    it('should convert tool_choice', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        tool_choice: { type: 'auto' },
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.tool_choice).toBe('auto');
    });
  });

  describe('Tool use conversion', () => {
    it('should convert assistant tool_use blocks to tool_calls', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Get the weather' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'toolu_123',
                name: 'get_weather',
                input: { city: 'NYC' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(2);
      const assistantMsg = result.messages[1] as any;
      expect(assistantMsg.role).toBe('assistant');
      expect(assistantMsg.tool_calls).toBeDefined();
      expect(assistantMsg.tool_calls[0].id).toBe('toolu_123');
      expect(assistantMsg.tool_calls[0].function.name).toBe('get_weather');
    });

    it('should preserve assistant thinking as reasoning_content for GLM/Qwen tool round-trips', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Inspect the file' },
          {
            role: 'assistant',
            content: [
              { type: 'thinking', thinking: 'Need to inspect the file before editing.' },
              {
                type: 'tool_use',
                id: 'toolu_read',
                name: 'Read',
                input: { path: 'src/index.ts' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'glm-5.2');
      const assistantMsg = result.messages[1] as any;

      expect(assistantMsg.reasoning_content).toBe('Need to inspect the file before editing.');
      expect(assistantMsg.content).toBeNull();
      expect(assistantMsg.tool_calls).toHaveLength(1);
    });

    it('should drop assistant thinking on generic tool round-trips', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Inspect the file' },
          {
            role: 'assistant',
            content: [
              { type: 'thinking', thinking: 'Do not send this to generic providers.' },
              {
                type: 'tool_use',
                id: 'toolu_read',
                name: 'Read',
                input: { path: 'src/index.ts' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4o');
      const assistantMsg = result.messages[1] as any;

      expect(assistantMsg.reasoning_content).toBeUndefined();
      expect(assistantMsg.content).toBeNull();
      expect(assistantMsg.tool_calls).toHaveLength(1);
    });

    it('should convert user tool_result blocks to tool messages', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_123',
                content: 'Sunny, 72°F',
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(1);
      expect(result.messages[0].role).toBe('tool');
      expect((result.messages[0] as any).tool_call_id).toBe('toolu_123');
      expect((result.messages[0] as any).content).toBe('Sunny, 72°F');
    });

    it('should handle tool_result with array content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_456',
                content: [
                  { type: 'text', text: 'Result 1' },
                  { type: 'text', text: 'Result 2' },
                ],
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect((result.messages[0] as any).content).toContain('Result 1');
      expect((result.messages[0] as any).content).toContain('Result 2');
    });

    it('should handle tool_result with is_error flag', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_789',
                content: 'Connection failed',
                is_error: true,
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect((result.messages[0] as any).content).toContain('Error:');
    });

    it('should handle tool_result with empty content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_empty',
                content: undefined as any,
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect((result.messages[0] as any).content).toBe('');
    });

    it('should handle mixed text and tool_result in user message', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_mix',
                content: 'Tool output',
              },
              { type: 'text', text: 'Now process this' },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      // Should have tool message + user message
      expect(result.messages).toHaveLength(2);
      expect(result.messages[0].role).toBe('tool');
      expect(result.messages[1].role).toBe('user');
    });

    it('should handle assistant with text and tool_use', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          {
            role: 'assistant',
            content: [
              { type: 'text', text: 'Let me check that.' },
              {
                type: 'tool_use',
                id: 'toolu_combo',
                name: 'search',
                input: { query: 'test' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      const assistantMsg = result.messages[0] as any;
      expect(assistantMsg.content).toBe('Let me check that.');
      expect(assistantMsg.tool_calls).toHaveLength(1);
    });
  });

  describe('Assistant prefill detection', () => {
    it('should skip assistant message with just { as content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Generate JSON' },
          { role: 'assistant', content: '{' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      // Should only have user message, assistant prefill skipped
      expect(result.messages).toHaveLength(1);
      expect(result.messages[0].role).toBe('user');
    });

    it('should skip assistant message with just [ as content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'List items' },
          { role: 'assistant', content: '[' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(1);
    });

    it('should skip assistant message with ``` as content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Write code' },
          { role: 'assistant', content: '```' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(1);
    });

    it('should skip assistant message with {" as content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Get data' },
          { role: 'assistant', content: '{"' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(1);
    });

    it('should keep assistant message with actual content', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Hello' },
          { role: 'assistant', content: 'Hello! How can I help you today?' },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(2);
      expect(result.messages[1].content).toBe('Hello! How can I help you today?');
    });

    it('should skip prefill in content block array', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'Generate JSON' },
          {
            role: 'assistant',
            content: [{ type: 'text', text: '{' }],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages).toHaveLength(1);
    });
  });

  describe('Tool ID handling', () => {
    it('should pass through tool_use IDs unchanged', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'First request' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'toolu_abc123',
                name: 'get_data',
                input: { query: 'first' },
              },
            ],
          },
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_abc123',
                content: 'First result',
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      // Should have all messages converted
      expect(result.messages.length).toBeGreaterThan(0);

      // Check that tool call ID is preserved
      const toolCalls = result.messages
        .filter((m: any) => m.tool_calls)
        .flatMap((m: any) => m.tool_calls);

      expect(toolCalls[0].id).toBe('toolu_abc123');
    });

    it('should generate unique ID for duplicate tool_use IDs (long IDs)', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'First request' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'toolu_duplicate_long_id',
                name: 'get_data',
                input: { query: 'first' },
              },
            ],
          },
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'toolu_duplicate_long_id',
                content: 'First result',
              },
            ],
          },
          { role: 'user', content: 'Second request' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'toolu_duplicate_long_id',
                name: 'get_data',
                input: { query: 'second' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      const toolCalls = result.messages
        .filter((m: any) => m.tool_calls)
        .flatMap((m: any) => m.tool_calls);

      // First and second tool call should have different IDs
      expect(toolCalls.length).toBe(2);
      expect(toolCalls[0].id).not.toBe(toolCalls[1].id);
    });

    it('should generate unique ID for duplicate tool_use IDs (short IDs)', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        messages: [
          { role: 'user', content: 'First request' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'short_id',
                name: 'get_data',
                input: { query: 'first' },
              },
            ],
          },
          {
            role: 'user',
            content: [
              {
                type: 'tool_result',
                tool_use_id: 'short_id',
                content: 'First result',
              },
            ],
          },
          { role: 'user', content: 'Second request' },
          {
            role: 'assistant',
            content: [
              {
                type: 'tool_use',
                id: 'short_id',
                name: 'get_data',
                input: { query: 'second' },
              },
            ],
          },
        ],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      const toolCalls = result.messages
        .filter((m: any) => m.tool_calls)
        .flatMap((m: any) => m.tool_calls);

      // First and second tool call should have different IDs
      expect(toolCalls.length).toBe(2);
      expect(toolCalls[0].id).not.toBe(toolCalls[1].id);
      // Short IDs should maintain similar length
      expect(toolCalls[1].id.length).toBe('short_id'.length);
    });
  });

  describe('Claude Code system prompt modification', () => {
    it('should replace Claude Code identifier with Claude Adapter branding', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        system:
          "You are Claude Code, Anthropic's official CLI for Claude. Here are more instructions.",
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].role).toBe('system');
      expect(result.messages[0].content).toContain('Claude Adapter');
      expect(result.messages[0].content).toContain(
        'https://github.com/shantoislamdev/claude-adapter'
      );
      expect(result.messages[0].content).toContain('https://claude-adapter.pages.dev/');
      expect(result.messages[0].content).toContain('Here are more instructions.');
      expect(result.messages[0].content).not.toContain("Anthropic's official CLI");
    });

    it('should preserve system prompts that do not contain Claude Code identifier', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        system: 'You are a helpful coding assistant.',
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].role).toBe('system');
      expect(result.messages[0].content).toBe('You are a helpful coding assistant.');
    });

    it('should handle system prompt as array with Claude Code identifier', () => {
      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        system: [
          { type: 'text', text: "You are Claude Code, Anthropic's official CLI for Claude." },
          { type: 'text', text: 'Additional context here.' },
        ],
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].role).toBe('system');
      expect(result.messages[0].content).toContain('Claude Adapter');
      expect(result.messages[0].content).toContain('Additional context here.');
    });
    it('should append update notification when update is available', () => {
      const mockUpdateInfo = { hasUpdate: true, current: '2.0.0', latest: '2.1.0' };
      (getCachedUpdateInfo as jest.Mock).mockReturnValue(mockUpdateInfo);

      const anthropicRequest: AnthropicMessageRequest = {
        model: 'claude-4.5-sonnet',
        max_tokens: 1024,
        system: "You are Claude Code, Anthropic's official CLI for Claude.",
        messages: [{ role: 'user', content: 'Hello' }],
      };

      const result = convertRequestToOpenAI(anthropicRequest, 'gpt-4');

      expect(result.messages[0].content).toContain(
        'IMPORTANT: A new version of Claude Adapter is available'
      );
      expect(result.messages[0].content).toContain('(2.0.0 → 2.1.0)');
      expect(result.messages[0].content).toContain('npm i -g claude-adapter');
    });
  });
});
