# Changelog

All notable changes to this project are documented in this file.

The format follows **Keep a Changelog** and this project adheres to **Semantic Versioning (SemVer)**.

---

## [2.0.1] — 2026-09-21

### Fixed

- Restore main's streaming/non-streaming thinking and client usage shapes, including reasoning-only responses and reasoning-field fallback.
- Stream independent tool calls immediately when their declared names are unambiguous; preserve fragmented names, tool/text indices and text after tools.
- Close each block once, retain final usage, distinguish explicit completion from truncated EOF, and reject damaged tool inputs without false success.

### Changed

- Replace handwritten upstream SSE framing with eventsource-stream 0.2.3 and downstream string concatenation with Axum Sse/Event.
- Add fixed-main differential and Anthropic SDK snapshot acceptance on Linux/Windows CI, first-content barrier tests, and benchmark SSE validation, per-kind latency and adapter-process RSS.

## [Unreleased]

### Fixed

- Accept repeated identical tool names while streaming their arguments; continue rejecting a changed name.
- Accept content-free choice tails and final usage after `finish_reason` without repeating downstream termination.
- Bound streaming waits for upstream headers, first body bytes, first Anthropic event and later body idle periods; fail timed-out streams without a false successful termination.

### Changed

- Verify real Claude Code streaming with isolated settings, a counted upstream route, and event arrival timing.

## [2.0.0] — 2026-09-20

### Added

- **Native proxy**: Moved HTTP, protocol conversion, SSE, tools, usage, and errors to a Rust service embedded in one offline npm package.
- **Credential configuration**: Added mutually exclusive `apiKeyEnv` support so the native service can resolve an API key from its inherited environment without storing the secret in `config.json`.
- **Offline distribution**: Bundled Linux x64 and Windows x64 binaries plus production JavaScript dependencies into one transferable tarball.
- **Protocol conversion**: Rebuilt the Rust request path against the established TypeScript converter so text, tools, assistant prefills, model options, and tool IDs retain the known-good OpenAI wire shape.
- **Responses**: Unified streaming and non-streaming finish reasons, added refusal `stop_details` and current usage fields, and kept third-party reasoning text private while preserving its token breakdown.
- **Bounded observability**: Added buffered, asynchronous JSONL writers with graceful flush and saturation accounting.

### Fixed

- **Third-party compatibility**: Restored JSON headers, bearer authorization, explicit stream mode, and unchanged forwarding of configured upstream headers such as `User-Agent`; unsupported image input now returns an explicit 400 while the text/tool path is stabilized.
- **Streaming**: Buffers split tool-call names and arguments, preserves sequential content-block indices, and reports missing final token usage as unknown rather than zero.
- **Diagnostics**: Records sanitized request-field shapes for upstream rejections without persisting prompts, tool inputs, schemas, headers, or credentials.

### Changed

- **Breaking**: `claude-adapter` is now CLI-only; JavaScript server, converter, and type exports were removed.
- **Performance**: Shared upstream connections and demand-driven streaming improve concurrent throughput and memory use.

### Compatibility

- Existing CLI flags and `~/.claude-adapter/config.json` continue to work without manual migration.

---

## [1.2.1] — 2026-06-24

### Fixed

- **Token Usage JSONL**: Changed persisted usage records to schema version 2 and now stores the upstream raw `usage` object.
- **Token Usage JSONL**: Removed converted usage fields from new records: `inputTokens`, `outputTokens`, `cachedInputTokens`, and `cacheCreationInputTokens`.

---

## [1.2.0] — 2025-12-22

### Added

- **Logging**: Added tracking for token usage (input/output/cache) and detailed error reporting.
- **Update System**: Added non-blocking update checks, smart upgrade prompts, and metadata storage.

### Improved

- **Performance**: Implemented zero-dependency update checks and race-safe JSON utilities to prevent CLI blocking.

---

## [1.1.5] — 2025-12-21

### Fixed

- **Azure OpenAI**: Adjusted prompt caching limits to comply with stricter provider constraints.

---

## [1.1.4] — 2025-12-20

### Fixed

- **ID Deduplication**: Reworked ID generation to preserve constraints while ensuring uniqueness.

---

## [1.1.3] — 2025-12-20

### Fixed

- **ID Handling**: Removed logic that caused ID mismatches across tool/result pairs.

---

## [1.1.2] — 2025-12-20

### Fixed

- **ID Format**: Initial fix for strict identifier formats (superseded by v1.1.3).

---

## [1.1.1] — 2025-12-20

### Fixed

- **API Compatibility**: Removed unsupported fields to prevent validation errors with strict providers.
- **Assistant Prefill**: Disabled prefill messages for providers that do not support them.

### Improved

- **Logging**: Simplified standard output while preserving details in debug mode.

---

## [1.1.0] — 2025-12-18

### Added

- **Core**: Added comprehensive request validation, ID tracing, and graceful server shutdown.
- **Logging**: implemented structured logging with timestamps and color support.
- **Docs**: Added complete API documentation.

### Improved

- **Internal**: Migrated to a high-performance web framework and significantly increased test coverage.

---

## [1.0.0] — 2025-12-17

### Added

- **Initial Release**: Launched **Claude Adapter** with CLI, proxy server, and persistent config.
- **Core Features**: Included Anthropic-to-OpenAI conversion, SSE streaming, and bidirectional tool support.
