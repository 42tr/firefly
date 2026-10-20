# Firefly UI SDK

为独立扩展模块提供页面消息桥、HTTP 适配器、Vue 组件和样式。Rust stdio MCP 适配器位于 [Rust SDK](../../crates/module-sdk)，接入协议见[模块文档](../../docs/modules.md)。

UI SDK 和应用共用 `frontend/package.json` 管理的依赖。可运行页面位于 `frontend/examples/rust-sdk-counter/`，完整构建与安装说明见 [Rust / UI SDK 计数器示例](../../examples/modules/rust-sdk-counter/README.md)。

从仓库根目录运行：

```sh
npm --prefix frontend ci
npm --prefix frontend run test:module-sdk
npm --prefix frontend run build:example:module-sdk
```
