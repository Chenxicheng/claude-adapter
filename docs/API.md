# HTTP API Reference

Claude Adapter 2.0 is a CLI product. It starts a native Rust proxy and does not export Node.js server or converter APIs.

## `POST /v1/messages`

Accepts an Anthropic Messages request and forwards it to the configured OpenAI-compatible Chat Completions endpoint. The maximum request body is 32 MiB.

```typescript
{
  model: string;
  max_tokens: number;
  messages: Array<{
    role: 'user' | 'assistant';
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
  };
}
```

Every response includes `x-request-id`.

Only `user` and `assistant` message roles are accepted. Assistant prefills are forwarded unchanged. Unsupported content blocks, Anthropic server tools, server-tool state, `allowed_callers`, `defer_loading`, and cache-only requests (`max_tokens: 0`) return `400` instead of being dropped or approximated.

Client tools may omit `type` or use `type: "custom"`; `name` must be non-empty and `input_schema` must be an object. Tool `strict` maps to the OpenAI function definition. Tool choices map as `none → none`, `auto → auto`, `any → required`, and named `tool → function`; `disable_parallel_tool_use: true` maps to `parallel_tool_calls: false`. `cache_control` is accepted but not converted—the configured OpenAI-compatible upstream remains responsible for any provider-specific caching.

Historical assistant `thinking` and `redacted_thinking` blocks are removed only when the same turn still contains text or a tool call. A turn that would become empty returns `400`.

### Images

Base64 sources become OpenAI data URLs:

```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "iVBORw0KGgo..."
  }
}
```

HTTP and HTTPS URL sources are forwarded without downloading or rewriting:

```json
{
  "type": "image",
  "source": {
    "type": "url",
    "url": "https://example.com/image.png"
  }
}
```

The adapter validates Base64, URL schemes, supported image MIME types, and required fields. `file_id` sources return `400`; use Base64 or an HTTP(S) URL. GIF content is forwarded as an image, without an animation guarantee.

For image-bearing `tool_result` blocks, text-only `role=tool` messages are emitted first. Images then appear in one `role=user` message labelled with the resolved `tool_call_id`. An image-only result receives an explicit `image follows` placeholder.

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
    input_tokens: number;
    output_tokens: number;
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

The final upstream usage chunk supplies the final usage values. If it is absent, final `input_tokens` is `null` rather than a fabricated zero. Private third-party `reasoning` or `reasoning_content` is not exposed as text or Anthropic thinking and is never logged; only `completion_tokens_details.reasoning_tokens` is mapped to `output_tokens_details.thinking_tokens`. A reasoning-only result is an upstream protocol error. Malformed streamed tool arguments emit an Anthropic `error` event and do not emit successful `message_delta` or `message_stop` events.

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
