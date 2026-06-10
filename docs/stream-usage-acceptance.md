# Stream Usage Acceptance

## Background

OpenAI-compatible streaming responses return complete usage data in a final usage chunk when `stream_options.include_usage` is enabled. Claude-style SSE sends `message_start` before content begins, so `message_start.message.usage.input_tokens` can be `0` before the final OpenAI usage chunk arrives.

This acceptance check verifies that the final `message_delta.usage` carries the completed input token count without delaying the stream start or changing unrelated request, response, or tool-call behavior.

## Acceptance Goals

- Native stream maps OpenAI `usage.prompt_tokens` to `message_delta.usage.input_tokens`.
- XML stream maps OpenAI `usage.prompt_tokens` to `message_delta.usage.input_tokens`.
- If no upstream usage chunk arrives, final `message_delta.usage` must not emit a synthetic `input_tokens: 0`.
- `message_start` remains the first event and is not delayed waiting for final usage.
- SSE event order remains unchanged.
- Text streaming, tool calls, stop reason mapping, `recordUsage`, and non-stream response logic remain unchanged.

## Non-Goals

- Do not estimate tokens locally with a tokenizer.
- Do not change request conversion.
- Do not call a real upstream API or use a real API key.
- Do not refactor unrelated code.

## Functional Check

Use mock stream chunks with final OpenAI usage:

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
- `message_delta.usage.cache_read_input_tokens === 8`
- `message_start` is still the first event

Native stream must also omit `message_delta.usage.input_tokens` when no upstream usage chunk arrives, while preserving a real upstream `prompt_tokens: 0`.

XML stream must satisfy the same final usage checks and keep `message_start` first.

## Verification Commands

```bash
npm test -- --runTestsByPath tests/streaming.test.ts tests/xmlStreaming.test.ts tests/response.test.ts tests/request.test.ts --runInBand
npm run build
npm run lint
```

## Review Checklist

- Diff only touches usage completion, types, tests, and this acceptance document.
- Stream first event is still not delayed.
- Native and XML stream paths expose final usage consistently.
- Non-stream usage mapping is not modified.
- No secrets, real network calls, or compatibility branches are introduced.
