# Rust migration specification

## Product boundary

`claude-adapter` 2.0 is a CLI product. One self-contained npm package owns offline installation, configuration prompts, embedded native binary selection, Claude settings updates, startup feedback, and signal forwarding. The Rust binary owns the listener, validation, upstream HTTP, Anthropic/OpenAI conversion, SSE, tools, usage/error storage, and runtime logging. There is no Node proxy fallback or JavaScript library API.

The existing `~/.claude-adapter/config.json` shape and CLI flags remain valid. Credentials use exactly one of the existing plaintext `apiKey` field or `apiKeyEnv`, which stores only a portable environment variable name. Rust resolves environment-backed credentials once at startup from the inherited process environment and never persists the value. The native process receives `--config <absolute-path>` and `--port <preferred>`, binds the first available loopback port, then prints one `CLAUDE_ADAPTER_READY=<json>` line after the listener is active. Node forwards Unix signals directly; on Windows it requests the same graceful drain through the child stdin pipe before using the 10-second forced-stop fallback.

## Protocol requirements

- Preserve `/health`, `POST /v1/messages`, CORS, Anthropic error envelopes, model-family options, tool ID repair, response/SSE event order, and usage schema version 2.
- Accept request bodies up to 32 MiB and at most 128 in-flight requests.
- Reuse one upstream HTTP client. Downstream polling must drive upstream polling so slow readers apply backpressure and disconnects cancel upstream work.
- Treat `main@8a19608` as the request-conversion oracle. Rust must emit the same Chat Completions JSON fields and role/block mapping as the TypeScript 1.2.2 converter before adding any Rust-only protocol behavior.
- Match the TypeScript transport contract: JSON `Accept`/`Content-Type`, bearer authorization, explicit `stream`, and unchanged forwarding of configured upstream headers, including `User-Agent` and provider-specific headers.
- Skip contentless hook messages, filter the same short assistant-prefill tokens, map non-user string/array turns as assistant history, and preserve the TypeScript tool ID repair and tool-result ordering behavior.
- Tools and tool choices follow the TypeScript converter exactly. Rust-only validation must not reject fields that TypeScript accepted or add fields that TypeScript did not send upstream.
- Image blocks in user messages or tool results return an explicit `400`; image conversion is deferred until the text/tool request path is stable.
- Use `max_completion_tokens` only for GPT-5 and OpenAI o-series models and `max_tokens` elsewhere; retain the Azure minimum-token adjustment. Do not forward `metadata.user_id`. Map `output_config.effort` only for recognized OpenAI reasoning models and preserve existing GLM/Qwen model-family behavior, including tool-turn reasoning history. Ignore `output_config.format` and other unconverted extensions, matching the TypeScript wire boundary.
- Forward the same optional fields as TypeScript and ignore unconverted provider extensions. Request validation protects required shapes but does not invent stricter upstream constraints.
- Restore main@8a19608 response and SSE behavior, including `reasoning_content || reasoning` as thinking blocks. Do not invent cryptographic signatures or log reasoning text. Reasoning-only completions are valid.
- Allocate consecutive Anthropic block indices exactly once; route interleaved tool deltas by upstream index. Text following tools is emitted immediately in a new block. Each block stops exactly once.
- Accumulate tool names against the request's declared names. Start a tool immediately on an exact, unambiguous match; defer only ambiguous names until finish_reason or [DONE]. Reject undeclared final names. Append arguments unchanged and validate the completed JSON object; absent arguments mean {}.
- On finish_reason, close content blocks but keep reading final usage. [DONE], or clean EOF after finish_reason, emits one message_delta and message_stop. [DONE] without finish_reason may infer tool_use/end_turn only for complete content. EOF without either terminator and transport errors fail without successful message_stop.
- Skip empty choices, accept usage-only chunks, and reject content after finish_reason. Preserve refusal handling and map length to max_tokens consistently. Index reuse, fragmented names, missing block stops and main's streaming length mapping are documented parity exceptions.
- Match main client usage: numeric zero placeholders at message_start; final output_tokens defaults to zero, input_tokens is omitted without token counters; non-streaming counters default to zero. Raw JSONL usage still distinguishes missing values from actual zeroes. Do not subtract cached or reasoning tokens from upstream totals.
- Use eventsource-stream 0.2.3 for upstream SSE framing and Axum Sse/Event::json_data for downstream encoding. Keep reqwest, Tokio, bytes and serde_json; do not add retrying POST wrappers or alternative JSON engines.

## JSONL storage

Usage and error records use a bounded Tokio channel with capacity 1024. The request path uses `try_send` and never waits for disk. One writer task keeps buffered daily usage and error files open, rotates at the UTC date boundary, flushes after 64 KiB or one second, and drains on graceful shutdown. A full queue drops observability records, increments a counter, and emits a coalesced warning. JSONL is observability data, not an audit or billing ledger.

For an upstream non-success response, error JSONL retains the status and response body plus a sanitized `upstreamRequestShape`. The shape includes only sorted field names, message roles/content types, token-field choice, tool count/function field names, tool-choice kind, model family, and stream mode. It never contains prompts, tool arguments or schemas, image data or URLs, headers, or API keys.

## Baseline and acceptance

A deterministic local upstream and shared fixtures compare Node 1.2 behavior with Rust. Benchmark concurrency is 1, 10, 50, and 100, with warm-up followed by five measured runs. Results record throughput, errors, p50/p95/p99 latency, first actual content latency, CPU, and RSS. Rust must preserve supported behavior and show a repeatable improvement in throughput, latency, or resource use without a material regression elsewhere.

`bench/baseline-2026-09-19.json` preserves the pre-cutover Node/Rust comparison. The current harness also runs tools, reasoning, upstream-error, slow-reader, and downstream-disconnect preflights before collecting text and SSE performance samples; `BENCH_UPSTREAM_DELAY_MS` controls deterministic upstream delay.

The first release embeds Linux glibc x64 and Windows x64 binaries. macOS, Linux arm64, musl, Windows arm64, Responses API, N-API, image proxying, and automatic model switching are out of scope.

Release CI builds and tests both targets, bundles their binaries and all production JavaScript dependencies into one `claude-adapter-2.0.0.tgz`, verifies an empty-cache `npm install --offline`, and creates the GitHub release without publishing to npm automatically.
