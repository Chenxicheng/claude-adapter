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
  stop_details?: { type: 'refusal'; category: null; explanation: string };
  stop_sequence: null;
  usage: {
    input_tokens: number;
    output_tokens: number;
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

`message_start.message.usage` uses numeric zero placeholders. Final `message_delta.usage.output_tokens` defaults to zero; `input_tokens` is omitted if no token counters arrived. With counters, missing individual counters default to zero, matching main@8a19608. Non-streaming counters also default to zero. Raw upstream JSONL usage remains authoritative and distinguishes missing usage from actual zero. Cache and reasoning token breakdowns are retained in JSONL, not synthesized into client usage.

Upstream `reasoning_content || reasoning` becomes `thinking` / `thinking_delta`, including reasoning-only completions. No cryptographic `signature` is fabricated. This preserves the established Claude Code adapter behavior, not native Anthropic signature integrity. Reasoning after the first tool is ignored as in main. Reasoning text is not written to usage/error logs.

Every content block receives a consecutive, unique index; deltas for parallel tools route by their upstream index, regardless of arrival order. Text after tools is streamed immediately in a new block. Each block stops exactly once. Tool names are accumulated against the request's declared names: an exact match with no longer matching name starts immediately. Ambiguous names wait only for that tool until finish_reason or [DONE]. Missing IDs are generated at block start; unknown final names fail. Arguments are streamed unchanged, then validated as a complete JSON object before block closure; absent arguments mean `{}`.

At finish_reason, content blocks close while the adapter continues reading final usage. `[DONE]`, or clean EOF following finish_reason, emits exactly one final message_delta/message_stop. An explicit `[DONE]` without finish_reason can infer end_turn/tool_use only for complete content. EOF without either terminal signal, malformed arguments, and transport errors emit `error` without successful final events. Empty choices are skipped; usage-only chunks are accepted. Content after finish_reason is an error.

Parity exceptions: main's reused tool/text indices, fragmented tool names, missing block stops, and streaming `length` mapping are corrected. Existing explicit refusal handling and validation of malformed upstream payloads remain. Unknown finish reasons are errors. SSE framing uses eventsource-stream 0.2.3; output uses Axum Sse/Event, with demand-driven streaming and cancellation rather than automatic POST replay.

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
    "HTTP-Referer": "https://example.com",
    "User-Agent": "Claude-Adapter/2.0"
  }
}
```

To keep the secret out of `config.json`, configure only its environment variable name:

```json
{
  "baseUrl": "https://api.openai.com/v1",
  "apiKeyEnv": "OPENAI_API_KEY",
  "models": {
    "opus": "gpt-4.1",
    "sonnet": "gpt-4.1",
    "haiku": "gpt-4.1-mini"
  }
}
```

Exactly one of `apiKey` and `apiKeyEnv` is required. Environment variable names must match
`[A-Za-z_][A-Za-z0-9_]*`; the variable must exist and be non-empty when the wizard runs and whenever
the adapter starts. The resolved value is never written back to configuration or logs.

Configured upstream headers are forwarded unchanged. `Authorization`, `Accept`, `Content-Type`, `Content-Length`, and `Host` remain adapter-controlled and cannot be configured through the CLI.

The CLI selects a native binary, waits for its ready record, and then updates Claude settings. The proxy chooses the requested port or the next available port atomically.
