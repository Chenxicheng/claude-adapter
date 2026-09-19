# Stream Usage Acceptance

## Background

OpenAI-compatible streaming responses return complete usage data in a final usage chunk when `stream_options.include_usage` is enabled. Claude-style SSE sends `message_start` before content begins, so `message_start.message.usage.input_tokens` can be `0` before the final OpenAI usage chunk arrives.

This acceptance check verifies that the final `message_delta.usage` carries the completed token counts without delaying the stream start or changing unrelated request, response, or tool-call behavior. `message_start.message.usage` is a Claude API compatibility placeholder in this adapter and must not be treated as recorded usage or cost data.

Official references:

- Claude streaming messages: https://platform.claude.com/docs/en/build-with-claude/streaming
- OpenAI chat completion streaming events: https://developers.openai.com/api/reference/resources/chat/subresources/completions/streaming-events/

## Usage Mapping

| OpenAI final usage field | Anthropic usage field |
| ------------------------ | --------------------- |
| `prompt_tokens`          | `input_tokens`        |
| `completion_tokens`      | `output_tokens`       |
| `completion_tokens_details.reasoning_tokens` | `output_tokens_details.thinking_tokens` |

`prompt_tokens_details.cached_tokens` is not mapped to Anthropic response usage
for the OpenAI Chat Completions path. It remains available in raw upstream usage
records for cache-hit and cost analysis.

## Acceptance Goals

- Native stream maps upstream prompt usage to full input context semantics:
  `input_tokens = prompt_tokens`.
- OpenAI `prompt_tokens_details.cached_tokens` must not be subtracted from
  `input_tokens` or exposed as Anthropic `cache_read_input_tokens`, because this
  adapter prioritizes Claude Code context-window safety over cache billing
  display compatibility.
- Native OpenAI Chat Completions usage does not include Anthropic cache token
  semantics, so `cache_creation_input_tokens` and `cache_read_input_tokens`
  are `null`; the adapter must not infer either value from OpenAI usage.
- OpenAI `completion_tokens_details.reasoning_tokens` is a breakdown within
  `completion_tokens`; it must not be subtracted from Anthropic `output_tokens`.
- If no upstream usage chunk arrives, final `message_delta.usage.input_tokens` is `null`, not a synthetic zero.
- If the final upstream usage chunk reports `prompt_tokens: 0`, final `message_delta.usage` must preserve the real `input_tokens: 0`.
- `message_start` remains the first event and is not delayed waiting for final usage.
- `message_start.message.usage` remains a transport compatibility placeholder and is not recorded.
- Streaming usage is recorded once at stream end with `usageStatus: "complete"` or `usageStatus: "missing_final_chunk"`.
- SSE event order remains unchanged.
- Text streaming and tool calls retain Anthropic event order. Streaming and non-streaming responses use the same finish-reason mapping.

## Non-Goals

- Do not estimate tokens locally with a tokenizer.
- Do not add a new public route or Responses API surface.
- Do not call a real upstream API or use a real API key.
- Do not refactor unrelated code.

## Functional Check

Use mock stream chunks with final OpenAI-compatible usage:

```json
{
  "prompt_tokens": 20,
  "completion_tokens": 10,
  "prompt_tokens_details": {
    "cached_tokens": 8
  }
}
```

Native stream must satisfy:

- `message_delta.usage.input_tokens === 20`
- `message_delta.usage.output_tokens === 10`
- `message_delta.usage.cache_read_input_tokens === null`
- `message_start` is still the first event

OpenAI completion usage breakdown must not reduce Anthropic output tokens:

```json
{
  "prompt_tokens": 120,
  "completion_tokens": 12,
  "prompt_tokens_details": {
    "cached_tokens": 80
  },
  "completion_tokens_details": {
    "reasoning_tokens": 7
  }
}
```

Native stream must then satisfy:

- `message_delta.usage.input_tokens === 120`
- `message_delta.usage.output_tokens === 12`
- `message_delta.usage.cache_read_input_tokens === null`
- `message_delta.usage.cache_creation_input_tokens === null`
- `message_delta.usage.output_tokens_details.thinking_tokens === 7`

Native stream must set `message_delta.usage.input_tokens` to `null` when no upstream usage chunk arrives, while preserving a real upstream `prompt_tokens: 0`.

Usage recording must satisfy:

- Non-stream responses record `usageStatus: "complete"` with real usage fields.
- Stream responses record usage only once, after the stream ends.
- Stream responses with final usage record `usageStatus: "complete"` with real usage fields.
- Stream responses without final usage record `usageStatus: "missing_final_chunk"` and omit unknown token fields.
- OpenAI usage records must not include `cacheCreationInputTokens`; native Chat Completions usage does not report cache creation tokens.
- Raw OpenAI usage records must preserve `prompt_tokens_details.cached_tokens`
  when upstream provides it.
- `message_start.message.usage` placeholder values are never persisted as token usage.

Reasoning privacy and integrity must satisfy:

- Upstream `reasoning` and `reasoning_content` text is neither displayed nor recorded.
- No `thinking`, `thinking_delta`, `signature`, or `signature_delta` is synthesized.
- A reasoning-only response produces an upstream protocol error.
- Streamed tool arguments must form one complete JSON object; malformed or scalar arguments produce an `error` SSE event without a successful `message_delta` or `message_stop`.

## Verification Commands

```bash
cargo test --manifest-path native/Cargo.toml stream::tests -- --nocapture
cargo test --manifest-path native/Cargo.toml converter::tests -- --nocapture
npm run build
npm run lint
cargo clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings
```

## Review Checklist

- Diff stays focused on native request conversion, usage completion, tests, and documentation.
- Stream first event is still not delayed.
- Native stream exposes final usage consistently.
- Non-stream and stream conversion omit upstream reasoning text while preserving its token breakdown.
- Usage records distinguish complete usage from a missing final usage chunk.
- Third-party reasoning traces are never exposed or logged.
- No secrets, real network calls, or compatibility branches are introduced.
