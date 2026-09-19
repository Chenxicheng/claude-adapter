# Changelog

All notable changes to this project are documented in this file.

The format follows **Keep a Changelog** and this project adheres to **Semantic Versioning (SemVer)**.

---

## [Unreleased]

---

## [2.0.0] — 2026-09-19

### Added

- **Native proxy**: Moved HTTP, protocol conversion, SSE, tools, usage, and errors to a Rust service shipped through platform-specific npm packages.
- **Protocol conversion**: Preserved assistant prefills, completed Anthropic tool-choice mapping, and now rejects unsupported roles, blocks, server tools, cache-only requests, and malformed upstream tool calls instead of silently dropping or approximating them.
- **Responses**: Unified streaming and non-streaming finish reasons, added refusal `stop_details` and current usage fields, and kept third-party reasoning text private while preserving its token breakdown.
- **Vision input**: Added ordered Base64 and URL image conversion, including explicitly associated tool-result screenshots.
- **Bounded observability**: Added buffered, asynchronous JSONL writers with graceful flush and saturation accounting.

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
