<div align="center">

# Claude Adapter

![Claude Adapter Logo](assets/banner.png)

**Adapt any model for Claude Code**

[![npm version](https://img.shields.io/npm/v/claude-adapter.svg)](https://www.npmjs.com/package/claude-adapter)
[![codecov](https://codecov.io/gh/shantoislamdev/claude-adapter/graph/badge.svg)](https://codecov.io/gh/shantoislamdev/claude-adapter)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Node.js Version](https://img.shields.io/node/v/claude-adapter.svg)](https://nodejs.org)

[Getting Started](#getting-started) •
[Installation](#installation) •
[Configuration](#configuration) •
[API Reference](#api-reference) •
[Contributing](#contributing)

</div>

---

## Overview

Unlock the full potential of Claude Code using **Claude Adapter**. It seamlessly connects Anthropic's powerful CLI to DeepSeek, GPT-Codex, Grok, and any OpenAI-compatible provider.

This adapter effectively "tricks" Claude Code into communicating with models it was not natively designed to support, handling all necessary protocol translations, header modifications, and payload restructuring in real-time.

### Key Features

- 🔄 **Protocol Translation Layer** — Implements a robust bi-directional conversion engine that maps Anthropic's message format to OpenAI's chat completion schema on the fly.
- 🌊 **Server-Sent Events (SSE) Streaming** — Provides full support for real-time response streaming, ensuring that the interactive feel of Claude Code is preserved even when backend by different models.
- 🛠️ **Tool Invocation Compatibility** — Seamlessly translates tool definitions and function call requests for upstream models that support native tool/function calling.
- ⚡ **Zero-Configuration Initialization** — Features an interactive CLI setup wizard that automates the generation of configuration files and environment variables.
- 🔌 **Transparent Proxying** — Operates non-intrusively as a local service, requiring no modifications to the core Claude Code binary or internal logic.

---

## Architecture

The adapter operates as a native Rust HTTP server that mimics the Anthropic API structure while forwarding requests to an upstream OpenAI-compatible target. One self-contained npm tarball includes the Linux x64 and Windows x64 binaries plus all production JavaScript dependencies, so it can be transferred and installed offline without Rust or registry access.

For broad third-party compatibility, the native request converter follows the `main` TypeScript 1.2.2 wire behavior and the effective OpenAI JavaScript SDK transport contract. Image requests are intentionally rejected until the text/tool path is stable. When an upstream request is rejected, local error JSONL includes a sanitized field-level request shape without recording prompts, tool inputs, schemas, headers, or credentials.

```
┌─────────────┐      ┌─────────────────┐      ┌─────────────────┐
│              ────▶                   ────▶                   │
│ Claude Code │      │  Claude Adapter │      │ OpenAI Endpoint │
│               ◀────                   ◀────                  │
└─────────────┘      └─────────────────┘      └─────────────────┘
   Anthropic              Converts                  OpenAI
    Format                Formats                   Format
```

When Claude Code initiates a request, **Claude Adapter** intercepts it, transforms the payload (including system prompts, message history, and tool definitions) into a format compliant with the OpenAI specification, and dispatches it to the configured backend (e.g., OpenAI, Grok, or a local inference server). The response is then captured, re-serialized into the Anthropic message format, and returned to the client.

---

## Getting Started

### Prerequisites

- **Runtime Environment**: Node.js Version 20.0.0 or higher is required to execute the adapter.
- **API Access**: A valid API key for an OpenAI-compatible service (e.g., OpenAI, DeepSeek, XAI).

### Installation

To install the adapter globally on your system, execute the following command:

```bash
npm install -g claude-adapter
```

For an offline machine, transfer the single release tarball and install it without registry access:

```bash
npm install -g --offline ./claude-adapter-2.0.0.tgz
```

The package selects its embedded binary at runtime. Version 2.0 supports Linux glibc x64 and Windows x64; macOS, Linux arm64, musl Linux, and Windows arm64 are not included.

### Quick Start

1. **Initialize the Service:**
   Launch the adapter's interactive setup utility:

   ```bash
   claude-adapter
   ```

2. **Configuration Wizard:**
   The CLI will guide you through the necessary configuration steps:
   - **Base URL**: Enter the endpoint URL of your compatible provider.
   - **Authentication**: Securely input your API key.
   - **Model Mapping**: Define which OpenAI-compatible models should be aliased to Claude's internal identifiers (`opus`, `sonnet`, `haiku`).
   - **Tool Support**: Upstream models must support native tool/function calling if you want Claude Code tools to work through the adapter.

3. **Operational State:**
   Once configured, the adapter will start a local proxy server. Claude Code is automatically reconfigured to route traffic through this local endpoint.

---

## Configuration

### CLI Options

The CLI accepts several flags to customize runtime behavior:

| Option                 | Description                                   | Default |
| ---------------------- | --------------------------------------------- | ------- |
| `-p, --port <port>`    | Specifies the port for the local proxy server | `3080`  |
| `-r, --reconfigure`    | Forces the specific reconfiguration workflow  | `false` |
| `--no-claude-settings` | Skip updating Claude Code settings files      | `false` |
| `-V, --version`        | Output the current version information        | —       |
| `-h, --help`           | Display available commands and options        | —       |

### Model Mapping Configuration

The core of the adapter's flexibility lies in its ability to map Claude's expected model tiers to arbitrary upstream models. This allows you to substitute, for example, a specialized coding model for `sonnet` or a high-speed inference model for `haiku`.

| Claude Tier | Intended Use Case | Example Mapping Strategy       |
| ----------- | ----------------- | ------------------------------ |
| `opus`      | Complex reasoning | `gpt-5.2-codex`, `glm-4.7`     |
| `sonnet`    | Balanced tasks    | `deepseek-3.2`, `minimax-m2.1` |
| `haiku`     | Low-latency ops   | `gpt-5-mini`, `gpt-oss-120b`   |

---

## API Reference

Version 2 is a CLI-only product. The previous JavaScript `createServer` and converter exports were removed so requests can go directly to the native service without a Node proxy layer. See the [HTTP API documentation](./docs/API.md) for the supported wire protocol.

---

## Supported Features

| Feature Capability    | Support Status | Implementation Notes                                     |
| --------------------- | :------------: | -------------------------------------------------------- |
| Text Generation       |       ✅       | Full fidelity                                            |
| System Prompts        |       ✅       | Mid-conversation instructions normalize to one system    |
| Real-time Streaming   |       ✅       | SSE event translation                                    |
| Tool/Function Calling |       ✅       | Bidirectional mapping                                    |
| Native Tool Support   |       ✅       | Upstream model must support native tool/function calling |
| Context Preservation  |       ✅       | Multi-turn history support                               |
| Token Limits          |       ✅       | Field selected by target model family                    |
| Sampling (Temp/Top P) |       ✅       | Parameter pass-through                                   |
| Stop Sequences        |       ✅       | Mapped to API equivalent                                 |
| Multimodal (Vision)   |       ✅       | Base64/URL input; upstream model must support vision     |

---

## Troubleshooting

### Common issues and Resolutions

<details>
<summary><strong>EADDRINUSE: Port collision</strong></summary>

The native service automatically selects the next available port at or above the requested port. Use `claude-adapter --port 3000` only when you want a different starting port.

</details>

<details>
<summary><strong>Authentication Failures</strong></summary>

If you encounter 401 errors, your API key may be invalid or expired. Rerun the configuration wizard:

```bash
claude-adapter --reconfigure
```

</details>

<details>
<summary><strong>Connection Refused</strong></summary>

Ensure the proxy server is running in a terminal window. Check your `~/.claude/settings.json` to verify the `ANTHROPIC_BASE_URL` is pointing to the correct local address (e.g., `http://localhost:3080`).

</details>

<details>
<summary><strong>Manual Configuration Mode</strong></summary>

To run the adapter without modifying Claude Code's settings, use `--no-claude-settings`:

```bash
claude-adapter --no-claude-settings
```

Then manually set environment variables in `~/.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://localhost:3080",
    "ANTHROPIC_AUTH_TOKEN": "default"
  }
}
```

</details>

---

## Development

To contribute to the codebase or build from source:

```bash
# Clone the repository
git clone https://github.com/shantoislamdev/claude-adapter.git
cd claude-adapter

# Install dependencies
npm install

# Build the native service and start the CLI
npm run build:native
npm run dev

# Execute both test suites
npm test
npm run test:native

# Check and build both layers
npm run build
npm run lint
npm run lint:native
```

Please refer to [CONTRIBUTING.md](./CONTRIBUTING.md) for comprehensive contribution guidelines.

---

## License

This project is licensed under the **MIT License**. For full terms and conditions, please see the [LICENSE](./LICENSE) file.

---

<div align="center">

**[Report Bug](https://github.com/shantoislamdev/claude-adapter/issues)** •
**[Request Feature](https://github.com/shantoislamdev/claude-adapter/issues)** •
**[Documentation](./docs/)**

Made with ❤️ by [Shanto Islam](https://shantoislamdev.web.app/)

</div>
