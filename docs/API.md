# API Reference

Complete API documentation for **Claude Adapter** — _Adapt any model for Claude Code_.

## Endpoints

### POST /v1/messages

The main API endpoint that accepts Anthropic Messages API requests and proxies them to an OpenAI-compatible backend.
Tool use requires an upstream model with native tool/function calling support.
This adapter currently targets the Chat Completions-style proxy path only. It does not expose a Responses API route yet.

**Request Headers:**

```
Content-Type: application/json
```

**Request Body:**

```typescript
{
  model: string;           // Required: Model name (passed through directly)
  max_tokens: number;      // Required: Maximum tokens in response
  messages: Message[];     // Required: Array of conversation messages
  system?: string;         // Optional: System prompt
  temperature?: number;    // Optional: 0-1, sampling temperature
  top_p?: number;          // Optional: 0-1, nucleus sampling
  stream?: boolean;        // Optional: Enable streaming responses
  thinking?: {
    type?: 'enabled' | 'disabled' | 'adaptive';
    budget_tokens?: number;
  };
  output_config?: {
    effort?: 'low' | 'medium' | 'high' | 'max';
  };
  stop_sequences?: string[]; // Optional: Stop sequences
  tools?: Tool[];          // Optional: Tool definitions
  tool_choice?: ToolChoice; // Optional: Tool selection preference
}
```

**Message Format:**

```typescript
{
  role: 'user' | 'assistant';
  content: string | ContentBlock[];
}
```

**Response (Non-streaming):**

```typescript
{
  id: string;
  type: 'message';
  role: 'assistant';
  content: ContentBlock[];
  model: string;
  stop_reason: 'end_turn' | 'max_tokens' | 'tool_use' | null;
  stop_sequence: string | null;
  usage: {
    input_tokens: number; // Fresh input only: excludes cache read/create tokens
    output_tokens: number;
    cache_read_input_tokens?: number;
    cache_creation_input_tokens?: number;
  };
}
```

**Response (Streaming):**
Server-Sent Events (SSE) with the following event types:

- `message_start` - Initial message metadata
- `content_block_start` - Start of a content block
- `content_block_delta` - Content update
- `content_block_stop` - End of a content block
- `message_delta` - Final message metadata with stop_reason
- `message_stop` - Stream complete

If an upstream vendor sends private reasoning traces such as `reasoning` or
`reasoning_content`, the adapter maps them to Anthropic-compatible `thinking`
blocks so Claude Code can display them. These blocks are display compatibility
only: the adapter does not generate fake Anthropic `signature` or
`signature_delta` fields.

## Model-Family Notes

- OpenAI Chat-compatible model families:
  - Streaming requests always send `stream_options.include_usage = true`.
  - `o*` and `gpt-5*` requests use `max_completion_tokens`.
  - OpenAI reasoning models can map Anthropic `thinking` / `output_config.effort`
    to `reasoning_effort`; `effort: "max"` maps to OpenAI `xhigh`.
- GLM-5:
  - `glm-5*` forwards Anthropic thinking controls as GLM `thinking`.
  - `glm-5.2*` can additionally receive GLM `reasoning_effort`.
  - Tool streaming is enabled automatically for streaming tool calls.
- Qwen3 / Qwen3.6:
  - Anthropic thinking requests are forwarded through `enable_thinking=true`.
  - In this adapter path, Qwen thinking requires `stream: true`.
- Generic OpenAI-compatible models:
  - The adapter does not send GLM/Qwen private fields unless the model name
    matches those model families.

---

### GET /health

Health check endpoint.

**Response:**

```json
{
  "status": "ok",
  "adapter": "claude-adapter"
}
```

---

## Converter Functions

### convertRequestToOpenAI

Converts an Anthropic Messages API request to OpenAI Chat Completions format.

```typescript
import { convertRequestToOpenAI } from 'claude-adapter';

const openaiRequest = convertRequestToOpenAI(anthropicRequest, 'gpt-4');
```

**Parameters:**

- `anthropicRequest: AnthropicMessageRequest` - The incoming request
- `targetModel: string` - The OpenAI model to use

**Returns:** `OpenAIChatRequest`

---

### convertResponseToAnthropic

Converts an OpenAI Chat Completion response to Anthropic format.

```typescript
import { convertResponseToAnthropic } from 'claude-adapter';

const anthropicResponse = convertResponseToAnthropic(openaiResponse, 'claude-4-opus');
```

**Parameters:**

- `openaiResponse: OpenAIChatResponse` - The OpenAI response
- `originalModelRequested: string` - Model name to include in response

**Returns:** `AnthropicMessageResponse`

---

### streamOpenAIToAnthropic

Transforms an OpenAI streaming response to Anthropic SSE format.

```typescript
import { streamOpenAIToAnthropic } from 'claude-adapter';

await streamOpenAIToAnthropic(openaiStream, fastifyReply, 'claude-4-opus');
```

---

## Error Responses

All errors follow Anthropic's error format:

```json
{
  "error": {
    "type": "invalid_request_error",
    "message": "Description of the error"
  }
}
```

**Error Types:**
| Status Code | Error Type |
| ----------- | ----------------------- |
| 400 | `invalid_request_error` |
| 401 | `authentication_error` |
| 403 | `permission_error` |
| 404 | `not_found_error` |
| 429 | `rate_limit_error` |
| 500 | `api_error` |

---

## Configuration Types

```typescript
interface AdapterConfig {
  baseUrl: string; // OpenAI-compatible API base URL
  apiKey: string; // API key for authentication
  models: {
    opus: string; // Model for Claude Opus requests
    sonnet: string; // Model for Claude Sonnet requests
    haiku: string; // Model for Claude Haiku requests
  };
  upstreamHeaders?: Record<string, string>; // Default headers sent to the upstream OpenAI-compatible API
}
```

---

## Example Usage

```typescript
import { createServer } from 'claude-adapter';

const config = {
  baseUrl: 'https://api.openai.com/v1',
  apiKey: process.env.OPENAI_API_KEY,
  models: {
    opus: 'gpt-4-turbo',
    sonnet: 'gpt-4',
    haiku: 'gpt-3.5-turbo',
  },
  upstreamHeaders: {
    'HTTP-Referer': 'https://example.com',
    'X-Title': 'Claude Adapter',
    'User-Agent': 'Claude-Adapter/2.2',
  },
};

const server = createServer(config);
await server.start(3080);

// Server now accepts Anthropic API requests at http://localhost:3080
```
