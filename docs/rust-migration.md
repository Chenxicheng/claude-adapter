# Rust migration specification

## Product boundary

`claude-adapter` 2.0 is a CLI product. One self-contained npm package owns offline installation, configuration prompts, embedded native binary selection, Claude settings updates, startup feedback, and signal forwarding. The Rust binary owns the listener, validation, upstream HTTP, Anthropic/OpenAI conversion, SSE, tools, usage/error storage, and runtime logging. There is no Node proxy fallback or JavaScript library API.

The existing `~/.claude-adapter/config.json` shape and CLI flags remain valid. The native process receives `--config <absolute-path>` and `--port <preferred>`, binds the first available loopback port, then prints one `CLAUDE_ADAPTER_READY=<json>` line after the listener is active. Node forwards Unix signals directly; on Windows it requests the same graceful drain through the child stdin pipe before using the 10-second forced-stop fallback.

## Protocol requirements

- Preserve `/health`, `POST /v1/messages`, CORS, Anthropic error envelopes, model-family options, tool ID repair, response/SSE event order, and usage schema version 2.
- Accept request bodies up to 32 MiB and at most 128 in-flight requests.
- Reuse one upstream HTTP client. Downstream polling must drive upstream polling so slow readers apply backpressure and disconnects cancel upstream work.
- Map Base64 and URL image sources to Chat Completions `image_url` parts without downloading or transforming them. Reject invalid fields and Anthropic `file_id` with an actionable 400.
- Tool messages remain text-only. Emit tool messages first, then one user image message whose text labels identify each resolved `tool_call_id`.
- Preserve assistant prefills. Reject unsupported roles, content blocks, cache-only requests, Anthropic server tools, and server-tool state instead of silently dropping or approximating them. Historical assistant thinking may be stripped only when visible text or tool use remains.
- Map all four Anthropic client `tool_choice` modes, `disable_parallel_tool_use`, and tool `strict`. Accept `cache_control` without conversion; provider-specific cache behavior belongs to the configured upstream.
- Map `max_tokens` exactly to `max_completion_tokens`, `metadata.user_id` to `safety_identifier`, `output_config.effort` to `reasoning_effort` (`max → xhigh`), and `output_config.format` to strict JSON Schema output. Reject unknown request fields and generic thinking modes that cannot be represented exactly; retain the existing explicit GLM/Qwen model-family behavior.
- Use one finish-reason mapping for streaming and non-streaming responses. Refusals remain text content with `stop_reason: "refusal"` and `stop_details`; unknown reasons and malformed tool calls are upstream protocol errors.
- Do not expose or persist third-party reasoning text. Preserve only its token breakdown, and reject reasoning-only responses. Streaming tool arguments are accumulated and validated as a complete JSON object before a successful stream terminator is emitted.
- Public usage reports the full OpenAI totals (`prompt_tokens` and `completion_tokens`) and null Anthropic cache fields. `message_start` uses numeric zero placeholders required by the Anthropic stream shape; missing final totals remain null, while real upstream zeroes remain zero. Raw upstream usage in JSONL is the source for cache, billing, and provider-specific analysis. `stop_sequence` remains null because Chat Completions does not distinguish a natural stop from a custom sequence match.

## JSONL storage

Usage and error records use a bounded Tokio channel with capacity 1024. The request path uses `try_send` and never waits for disk. One writer task keeps buffered daily usage and error files open, rotates at the UTC date boundary, flushes after 64 KiB or one second, and drains on graceful shutdown. A full queue drops observability records, increments a counter, and emits a coalesced warning. JSONL is observability data, not an audit or billing ledger.

## Baseline and acceptance

A deterministic local upstream and shared fixtures compare Node 1.2 behavior with Rust. Benchmark concurrency is 1, 10, 50, and 100, with warm-up followed by five measured runs. Results record throughput, errors, p50/p95/p99 latency, first actual content latency, CPU, and RSS. Rust must preserve supported behavior and show a repeatable improvement in throughput, latency, or resource use without a material regression elsewhere.

`bench/baseline-2026-09-19.json` preserves the pre-cutover Node/Rust comparison. The current harness also runs tools, reasoning, upstream-error, slow-reader, and downstream-disconnect preflights before collecting text and SSE performance samples; `BENCH_UPSTREAM_DELAY_MS` controls deterministic upstream delay.

The first release embeds Linux glibc x64 and Windows x64 binaries. macOS, Linux arm64, musl, Windows arm64, Responses API, N-API, image proxying, and automatic model switching are out of scope.

Release CI builds and tests both targets, bundles their binaries and all production JavaScript dependencies into one `claude-adapter-2.0.1.tgz`, verifies an empty-cache `npm install --offline`, and creates the GitHub release without publishing to npm automatically.
