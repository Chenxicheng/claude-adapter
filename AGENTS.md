# Repository Guidelines

## 项目结构与模块组织

`src/cli.ts` 是 npm CLI 入口，只负责配置交互、选择并启动平台 Rust 二进制、进程信号与错误反馈。`native/` 是完整代理服务，负责 HTTP、协议转换、SSE、usage/error JSONL 和运行时日志。`npm/` 保存平台二进制包定义，包内只放对应目标的可执行文件。Node 不代理请求，也不提供 Rust 服务的 fallback。`tests/` 保存 CLI 与分发测试，Rust 单元和集成测试放在 `native/` 对应模块及 `native/tests/`。`bench/` 保存确定性 mock upstream、fixtures 和 benchmark harness。`docs/` 保存 API、迁移规格与验收说明。

## 构建、测试与开发命令

- `npm install`：安装依赖；要求 Node.js `>=20.0.0`。
- `npm run dev`：通过 `ts-node` 本地运行 CLI。
- `npm run build`：编译 npm CLI 到 `dist/`。
- `npm test`：运行 Jest 测试套件。
- `npm test -- --runInBand`：串行运行完整测试，提交前优先使用。
- `npm run lint`：运行 npm CLI 与测试的 ESLint 检查。
- `npm run format`：用 Prettier 格式化 npm CLI 与测试文件。
- `cargo test --manifest-path native/Cargo.toml`：运行 Rust 测试。
- `cargo fmt --manifest-path native/Cargo.toml --check`：检查 Rust 格式。
- `cargo clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings`：将 Rust lint warning 视为失败。
- `cargo build --manifest-path native/Cargo.toml --release`：构建本机 release 二进制。

## 编码风格与命名约定

TypeScript 使用 strict mode、两空格缩进、单引号和分号；它不得包含协议转换或请求代理逻辑。Rust 使用 `rustfmt` 默认格式；协议转换与 HTTP handlers 分离，共享状态只保存线程安全的 client、配置和 bounded writer。配置和内部状态使用明确类型；协议 wire payload 使用 `serde_json::Value` 保留供应商扩展字段。不新增兼容层、无界队列或每请求 client。

## 测试规范

Node 测试使用 Jest + `ts-jest`，Rust 测试使用内置 test harness 与 `tokio::test`。协议变更必须更新共享 fixtures 和 Rust 测试；CLI、分发或配置变更必须更新 Jest 测试。集成测试使用本地 mock upstream，不依赖真实 API key、真实网络请求或外部服务状态。benchmark 必须固定 payload、upstream 延迟、日志配置和构建模式。

## Commit 与 Pull Request 规范

提交信息遵循历史中的 Conventional Commits，例如 `fix(proxy): ...`、`feat(config): ...`、`refactor(storage): ...`。每个 commit 只包含一个逻辑变更。提交前运行 npm 全套检查、Rust 全套检查、release build 和 `git diff --check`。协议或分发变化必须同步测试、README、API 文档和 CHANGELOG。不要提交密钥、token、`.env`、`dist/`、`target/` 或生成的 release artifacts。
