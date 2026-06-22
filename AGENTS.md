# Repository Guidelines

## 项目结构与模块组织
`src/cli.ts` 是 CLI 入口，`src/index.ts` 导出库 API。Fastify 代理启动与请求处理位于 `src/server/`。Anthropic 与 OpenAI 之间的请求、响应、流式事件、工具和 usage 转换逻辑放在 `src/converters/`。API、配置和协议类型放在 `src/types/`。配置、日志、校验、存储、metadata 和 token usage 等通用逻辑放在 `src/utils/`。测试统一放在 `tests/*.test.ts`。`docs/` 保存 API 与验收说明，`assets/` 保存静态图片资源。

## 构建、测试与开发命令
- `npm install`：安装依赖；要求 Node.js `>=20.0.0`。
- `npm run dev`：通过 `ts-node` 本地运行 CLI。
- `npm run build`：将 TypeScript 编译到 `dist/`。
- `npm test`：运行 Jest 测试套件。
- `npm test -- --runInBand`：串行运行完整测试，提交前优先使用。
- `npm run lint`：对 `src/**/*.ts` 和 `tests/**/*.ts` 运行 ESLint。
- `npm run format`：用 Prettier 格式化源码和测试文件。

## 编码风格与命名约定
项目使用 TypeScript strict mode。协议行为应放在 converters 中，不要塞进 server handlers。导出函数和公共函数优先写清参数与返回类型。遵循现有两空格缩进、单引号和分号规则。函数与变量使用 `camelCase`，接口、类型和类使用 `PascalCase`，常量仅在需要全局常量语义时使用 `UPPER_SNAKE_CASE`。

## 测试规范
测试框架是 Jest + `ts-jest`。修改 converters、streaming、usage、handlers、config 或 validation 时，必须新增或更新对应测试。测试文件命名为 `*.test.ts`，保持现有“按行为断言”的风格。使用 mock 模拟上游 API；不要依赖真实 API key、真实网络请求或外部服务状态。

## Commit 与 Pull Request 规范
提交信息遵循历史中的 Conventional Commits，例如 `fix(proxy): ...`、`feat(config): ...`、`refactor(storage): ...`。每个 commit 只包含一个逻辑变更。提交前运行 `npm test -- --runInBand`、`npm run build`、`npm run lint` 和 `git diff --check`。PR 需要说明行为变化、验证命令、相关 issue，并在功能变化时同步更新测试和文档。不要提交密钥、token、`.env` 文件或生成的 `dist/`，除非发布流程明确要求。
