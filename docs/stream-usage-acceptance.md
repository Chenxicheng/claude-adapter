# Response and stream acceptance

## Contract and layout

`main@8a19608` is the normal-path oracle. `tests/protocol-parity.mjs` loads its converters from Git in memory and compares complete events after normalizing generated message IDs. It also consumes the same streams using Anthropic SDK 0.71.2 final snapshots; convenience callbacks are not the oracle for interleaved blocks. `bench/stream-validation.mjs` validates lifecycle, block identity, tool JSON, usage and termination for both acceptance and benchmarks. Fixtures remain under `bench/fixtures/`; sanitized real-upstream evidence remains under `tests/real-e2e/results/`.

Rules live in [API.md](API.md) and [rust-migration.md](rust-migration.md). Thinking is forwarded, not logged or signed. Non-streaming usage has numeric input/output totals. Stream starts use zero placeholders; final output defaults to zero and input is omitted without counters. Do not subtract cached or reasoning tokens from full upstream totals; retain raw usage and missing-final-chunk status in JSONL.

Explicit exceptions to main are unique content indices, complete fragmented names, paired block stops, truncated-EOF errors, malformed tool JSON rejection and `length -> max_tokens`. Request-side image rejection and existing refusal/unknown-reason validation remain. No alternative protocol modes or Node fallback are added.

## Deterministic checks

- Rust barrier tests send a single thinking/text/tool increment then wait for a signal. Downstream must receive the increment before releasing upstream. This proves progress without flaky timing assumptions.
- Test CR, LF, CRLF, byte-split UTF-8, reasoning alias fallback, reasoning-only output, parallel and out-of-order tool indices, fragmented/ambiguous names, no-argument tools, text after tools and final usage.
- Check explicit [DONE] without finish_reason, EOF after finish_reason, truncated EOF, malformed arguments, undeclared names and upstream errors. Failure never fabricates successful message_stop.
- Benchmark preflights verify slow-reader backpressure, client-disconnect cancellation and active-stream shutdown. HTTP 200 without valid content and message_stop is a failure.

```sh
cargo test --manifest-path native/Cargo.toml
cargo build --manifest-path native/Cargo.toml --release
npm run test:protocol
npm test -- --runInBand
npm run lint
npm run build
cargo fmt --manifest-path native/Cargo.toml --check
cargo clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings
git diff --check
```

`npm run test:protocol` requires the fixed Git object (CI checks out full history) and the release binary. `ADAPTER_BINARY` can select a platform build. Native-platform CI runs it on Linux x64 and Windows x64; a local macOS pass is not proof of either target.

## Performance comparison

Use identical machine, payload, logging and release mode. Run targets sequentially with concurrency 1/10/50/100, five repetitions, warm-up, and upstream delays 0 and 100 ms. Keep baseline binaries outside the repository. Never treat dropped thinking as an optimization.

```sh
BENCH_SUMMARY=1 BENCH_UPSTREAM_DELAY_MS=0 node bench/benchmark.mjs
BENCH_SUMMARY=1 BENCH_UPSTREAM_DELAY_MS=100 node bench/benchmark.mjs
BENCH_WORKLOAD=reasoning-tool BENCH_SUMMARY=1 node bench/benchmark.mjs
BENCH_TARGET=rust-before BENCH_BINARY=/absolute/path/to/baseline node bench/benchmark.mjs
BENCH_TARGET=node BENCH_NODE_DIST=/absolute/path/to/main/dist node bench/benchmark.mjs
```

`BENCH_WORKLOAD` is text (default) or reasoning-tool; run both delays for the latter too. Old Rust is compared only on text because it deliberately omitted reasoning. `rust-before` skips new-behavior preflights, but measured responses still undergo lifecycle/content checks. TS runs as a separate process. Record first thinking/tool/text and overall first-content latency, throughput, p50/p95/p99 and sampled adapter RSS. `harnessRssBytes` is explicitly separate. RSS sampling is a process high-water observation, not per-request allocation profiling; unavailable measurements are null.

Recheck throughput decreases over 10% or p95 increases over max(10%, 1 ms), then diagnose repeatable regressions. Parser microbenchmarks and end-to-end latency must not be conflated. The memory check for long streams must distinguish retained active tool arguments (needed for validation) from unbounded accumulation of already-forwarded text.

## Real upstream

Run the manual harness described in [tests/real-e2e/README.md](../tests/real-e2e/README.md) with external credentials. Verify actual thinking, tool execution and final answer in Claude Code. If the provider sends no reasoning, record that limitation; a mock pass must not be labeled a real-provider thinking pass. Committed results contain only sanitized status/model/event/token summaries, never credentials, prompts or responses.

## Recorded acceptance: 2026-09-21

On macOS arm64, 42 Rust tests, 91 Jest tests and 19 protocol/SDK cases passed. The real Qwen run received thinking as its first content delta; Claude Code emitted thinking events, executed Read and returned the expected final answer. See the [sanitized result](../tests/real-e2e/results/20260921T125117Z-qwen-qwen3.6-35b-a3b.json). This checks Claude Code's stream output, not a visual terminal screenshot. The earlier failed connection attempt is recorded separately.

The [performance artifact](../bench/response-parity-2026-09-21.json) contains all ten five-repetition matrices and binary hashes. All measured responses passed validation. All 16 text comparisons against pre-fix Rust passed the throughput/p95 gates. At zero upstream delay, streaming throughput changed by +1.6% to +4.4%; these small differences do not establish a significant speedup. Thinking/tool streaming throughput was 0.97–1.12 times main TS at zero delay and 0.96–1.01 times at 100 ms. These are local mock measurements, not model inference acceleration.

A separate single-request memory probe forwarded 16 MiB and then 64 MiB of text in 64 KiB upstream deltas, discarded client output after checking successful termination, and sampled only the adapter process every 25 ms. Sampled peak RSS was 11,370,496 and 12,337,152 bytes respectively (growth 966,656 bytes); this supports bounded text retention for this workload, not a universal allocation bound. Benchmark preflights also passed slow-reader, disconnect and graceful-shutdown checks.

Linux x64 and Windows x64 build/protocol checks are wired into CI but were not executed in this macOS-only validation environment. Those platform results remain pending; no release or push was performed.
