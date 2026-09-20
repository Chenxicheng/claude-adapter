# HTTP API Reference

Claude Adapter 2.0 is a CLI product. It starts a native Rust proxy and does not export Node.js server or converter APIs.

## `POST /v1/messages`

Accepts an Anthropic Messages request and forwards it to the configured OpenAI-compatible Chat Completions endpoint. The maximum request body is 32 MiB.

```typescript
{
  model: string;
  max_tokens: number;
  messages: Array<{
    role: 'user' | 'assistant' | 'system';
    content: string | ContentBlock[];
  }>;
  system?: string | ContentBlock[];
  temperature?: number;
  top_p?: number;
  stream?: boolean;
  stop_sequences?: string[];
  tools?: Tool[];
  tool_choice?: ToolChoice;
  thinking?: {
    type?: 'enabled' | 'disabled' | 'adaptive';
    budget_tokens?: number;
  };
  output_config?: {
    effort?: 'low' | 'medium' | 'high' | 'max';
    format?: {
      type: 'json_schema';
      schema: Record<string, unknown>;
    };
  };
  cache_control?: { type: 'ephemeral'; ttl?: '5m' | '1h' } | null;
  metadata?: { user_id?: string | null };
}
```

Every response includes `x-request-id`.

Request conversion follows `main@8a19608` (TypeScript 1.2.2). `user` maps to OpenAI `user`; other non-empty string roles map to assistant history. Messages with missing or null content are skipped, and the same short assistant-prefill tokens are filtered. GPT-5 and OpenAI o-series models use `max_completion_tokens`; other models use `max_tokens`, with the existing Azure minimum adjustment. Required request shapes are validated, while unknown fields not converted by TypeScript are ignored instead of forwarded.

Upstream requests explicitly send `Accept: application/json`, `Content-Type: application/json`, bearer authorization, `stream: true | false`, and an OpenAI JavaScript SDK-compatible user agent.

Client tools map only `name`, `description`, and `input_schema`. Tool choices map as `auto → auto`, `any → required`, named `tool → function`, and other values → `auto`, matching the TypeScript converter. Tool-result text becomes `role=tool` messages in input order.

For GPT-5 and OpenAI o-series models, `output_config.effort` and Anthropic thinking budgets map to the same `reasoning_effort` values as the TypeScript converter. GLM/Qwen retain their provider-specific options and tool-turn `reasoning_content`. Other historical thinking is not forwarded; a thinking-only historical turn becomes an assistant message with null content, matching TypeScript wire behavior. `output_config.format` and `metadata.user_id` are not forwarded. Stop arrays, including empty arrays, are forwarded as `stop`.

### Images

Image blocks, including images nested in `tool_result`, currently return `400`. Image conversion will be reintroduced only after the text/tool request path is stable.

### Non-streaming response

```typescript
{
  id: string;
  type: 'message';
  role: 'assistant';
  content: ContentBlock[];
  model: string;
  stop_reason: 'end_turn' | 'max_tokens' | 'tool_use' | 'refusal';
  stop_details: { type: 'refusal'; category: null; explanation: string } | null;
  stop_sequence: string | null;
  container: null;
  usage: {
    input_tokens: number | null;
    output_tokens: number | null;
    cache_creation_input_tokens: null;
    cache_read_input_tokens: null;
    output_tokens_details: { thinking_tokens: number } | null;
    server_tool_use: null;
    cache_creation: null;
    inference_geo: null;
    service_tier: null;
  };
}
```

Finish reasons map as `stop → end_turn`, `length → max_tokens`, `tool_calls → tool_use`, and `content_filter → refusal`. A refusal remains a normal text content block and includes `stop_details`. Unknown or legacy `function_call` finish reasons, missing tool names, and tool arguments that are not a complete JSON object are upstream protocol errors (`502`). The adapter cannot distinguish a natural OpenAI `stop` from a custom stop-sequence match, so `stop_sequence` remains `null`.

### Streaming response

Streaming uses SSE and preserves Anthropic event order:

- `message_start`
- `content_block_start`
- `content_block_delta`
- `content_block_stop`
- `message_delta`
- `message_stop`

`message_start.message.usage` uses the Anthropic-compatible numeric placeholder `0` because final usage is not known yet. The final upstream usage chunk supplies the final values. If it is absent, final token counts remain `null` rather than fabricated zeroes; a real upstream zero remains `0`. Private third-party `reasoning` or `reasoning_content` is not exposed as text or Anthropic thinking and is never logged; only `completion_tokens_details.reasoning_tokens` is mapped to `output_tokens_details.thinking_tokens`. A reasoning-only result is an upstream protocol error. Malformed streamed tool arguments emit an Anthropic `error` event and do not emit successful `message_delta` or `message_stop` events.

Tool names may arrive in multiple upstream deltas. The adapter waits until the name is stable, then streams accumulated and subsequent argument fragments immediately; the complete arguments are still validated as a JSON object before successful termination. Upstream SSE error payloads are recognized before normal choice parsing and retained in error JSONL.

## `GET /health`

```json
{
  "status": "ok",
  "adapter": "claude-adapter",
  "dropped_jsonl_records": 0
}
```

The dropped counter increases when the bounded observability queue cannot accept another JSONL record. Proxy traffic is not blocked by observability writes.

## Errors

```json
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "message": "Description of the error"
  }
}
```

| Status   | Error type              |
| -------- | ----------------------- |
| 400, 413 | `invalid_request_error` |
| 401      | `authentication_error`  |
| 403      | `permission_error`      |
| 404      | `not_found_error`       |
| 429      | `rate_limit_error`      |
| 500+     | `api_error`             |

## Configuration

The existing configuration file remains compatible:

```json
{
  "baseUrl": "https://api.openai.com/v1",
  "apiKey": "...",
  "models": {
    "opus": "gpt-4.1",
    "sonnet": "gpt-4.1",
    "haiku": "gpt-4.1-mini"
  },
  "upstreamHeaders": {
    "HTTP-Referer": "https://example.com"
  }
}
```

The CLI selects a native binary, waits for its ready record, and then updates Claude settings. The proxy chooses the requested port or the next available port atomically.
