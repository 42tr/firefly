# Firefly Module SDK

为独立扩展模块提供 Rust stdio MCP 适配器，页面消息桥和 UI 组件位于 [UI SDK](../../frontend/module-sdk)。主程序通过 MCP 连接模块，不依赖此 SDK；模块作者可以选择使用它。

可运行示例见 [Rust / UI SDK 计数器](../../examples/modules/rust-sdk-counter/README.md)，完整安装与开发协议见 [模块文档](../../docs/modules.md)。

在仓库根目录构建示例：

```sh
npm --prefix frontend ci
npm --prefix frontend run build:example:module-sdk
```

Rust 示例直接依赖本 crate，使用 `config()`、`data_dir()`、`db::options()` 和 `App::serve()`；页面源码位于 `frontend/examples/rust-sdk-counter/`，导入 `frontend/module-sdk/` 中的桥、HTTP 适配器和样式。产物是可独立安装的模块包。
