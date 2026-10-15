# Rust / UI SDK 计数器示例

这是 `firefly-module-sdk` 的实际使用示例。模块运行在独立 Rust 进程中，以 stdio MCP 提供查询、增加和清零工具；页面通过 SDK 的消息桥调用工具，计数保存在模块自己的 SQLite 中。

## 构建

在 Firefly 仓库根目录执行，需要仓库固定的 Rust 工具链与 Node.js 24：

```sh
npm ci
npm run build:example:module-sdk
```

产物位于 `output/modules/rust-sdk-counter/`，包含可执行文件、`module.toml`、配置 Schema 和单文件 `ui.html`。页面构建直接导入 `crates/module-sdk/ui/bridge.js`、`utils/http.js` 和样式；使用根目录已有的 Vite，不新增前端依赖。

构建发布版可执行文件：

```sh
cargo build --release --locked -p firefly-sdk-counter-example
node examples/modules/rust-sdk-counter/build.mjs --release
```

可执行文件需要在与运行 Firefly 相同的操作系统和架构上构建。模块安装后不需要 Rust、Node.js 或源码。

## 安装与使用

将完整产物目录复制到 Firefly 运行目录的 `MODULES_DIR` 下。首次安装且目标目录不存在时，例如：

```sh
mkdir -p modules
cp -R output/modules/rust-sdk-counter modules/rust-sdk-counter
```

在“设置 → 扩展模块”重新扫描，选择“Rust SDK 示例”，配置名称和步长后启用。随后从“更多功能 → SDK 计数器”打开页面。

- 点击“增加”，按配置的步长增加计数；“清零”重置为 0。
- 关闭页面、停用再启用或重启 Firefly，计数仍然保留。
- 更改步长并保存会重启模块，新步长生效，已有计数保留。
- 如需 Agent 调用，在“Agent 配置”刷新工具并分配本模块的 `read`、`request` 工具。

默认数据位置为 `storage/modules/rust-sdk-counter/counter.sqlite3`，具体目录由宿主通过 `FIREFLY_MODULE_DATA_DIR` 注入。

## 代码对应关系

| SDK 能力 | 示例用法 |
| --- | --- |
| `sdk::config()` | 读取宿主传入的名称和步长 |
| `sdk::data_dir()`、`sdk::db::options()` | 打开模块自己的 SQLite，启用 WAL 与外键 |
| `sdk::init_logging()` | 日志写入 stderr，stdout 留给 MCP |
| `sdk::App::serve()` | 将 Axum 路由适配为 stdio MCP 工具和页面资源 |
| UI `initialize()`、`apiFetch()` | 页面初始化，通过消息桥读写模块内部路由 |
| UI `closeApp()`、`onClose()` | 页面关闭与宿主关闭请求 |

SDK 自动提供 `open_app`、`read`、`request`。模型或页面调用示例：

```json
{"name":"read","arguments":{"path":"/counter"}}
{"name":"request","arguments":{"method":"POST","path":"/increment"}}
{"name":"request","arguments":{"method":"POST","path":"/reset"}}
```

`src/main.rs` 中的 Axum Router 只在模块进程内处理这些请求，不监听 HTTP 端口。此示例没有定时任务、公开下载或状态提示；需要这些能力时可增加对应的 SDK 配置和清单声明。

## 验证

```sh
cargo test --locked -p firefly-sdk-counter-example
cargo clippy --locked -p firefly-sdk-counter-example --all-targets -- -D clippy::correctness
```

测试启动真实模块进程，验证 MCP 初始化、工具及资源、配置步长、只读调用边界，以及重启后数据保留。完整页面和宿主交互可在构建 Firefly 与模块包后运行：

```sh
npm run build
cargo build --locked
npm run build:example:module-sdk
python3 scripts/test_rust_sdk_module_ui.py
```

复制到独立仓库时，将 Cargo 的 workspace 继承改为明确字段和依赖版本，调整 `firefly-module-sdk` 的路径或 Git 依赖，并为页面构建配置 SDK 的实际位置。
