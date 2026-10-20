# 外部模块开发与安装

Firefly 可以在运行时加载独立 MCP 模块，无需修改 Rust 或 Vue 源码，也无需重新编译 Firefly。模块提供工具，以及可选的页面、设置、状态提示和定时任务。模块默认停用，在“侧栏左下角设置 → 扩展模块”中配置并启用，再到“Agent 配置”中将工具分配给主 agent 或自建子 agent。

本版本接入 Firefly Agent、模块页面和宿主调度器；Codex、Claude CLI 的 MCP 配置仍由各自管理。页面支持 MCP Apps 的初始化、工具输入输出通知和本模块工具调用，使用打包为单文件的 HTML。宿主没有声明的能力会返回方法不支持；暂不提供外部资源联网、主站消息写入、采样、第三方模块访问核心数据库或通用宿主 API。

## 安装示例

默认 `MODULES_DIR=modules`，相对 Firefly 工作目录解析。将完整模块包复制进去，然后在界面点击“重新扫描”。例如在 Firefly 工作目录执行：

```sh
mkdir -p modules
cp -R examples/modules/calendar modules/calendar
cp frontend/examples/calendar/calendar.html modules/calendar/calendar.html
```

示例源码路径相对于本仓库；只下载 Firefly 二进制时，另行下载或复制模块包。示例需要 Python 3.9 或更新版本及系统时区数据。示例不包含任何个人日期，也不会自动开启定时检查。

部署在容器中时，将模块包目录挂载到 `MODULES_DIR`，将模块数据目录挂载到 `MODULE_DATA_DIR`。stdio 模块的运行时必须位于 Firefly 所在容器内；HTTP 模块可以使用另一个容器，并填写 Firefly 可访问的服务地址。

启用“重要日期示例”后，更多功能菜单会出现“我的重要日期”。在“Agent 配置”中刷新工具、勾选日期工具并保存后，所选 agent 就能调用。在模块管理页可检查工具、提交参数进行测试，以及查看自动任务结果。自动检查只记录当天匹配的日期，不发送邮件或推送通知。

`examples/modules/echo/` 是不带页面的最小工具模板；日期示例的服务端、配置 Schema、SQLite 和调度声明在 `examples/modules/calendar/`，页面源码在 `frontend/examples/calendar/calendar.html`。按上面的命令组装后，完整模块包可以复制到独立仓库，业务逻辑与 Firefly 源码没有依赖。示例使用少量 Python 标准库代码演示 MCP；复杂模块建议使用官方 MCP SDK。

[`examples/modules/rust-sdk-counter/`](../examples/modules/rust-sdk-counter/README.md) 是直接使用本仓库 Rust / UI SDK 的计数器示例，展示配置读取、SQLite 持久化、Axum 路由到 MCP 的适配，以及页面消息桥。根目录运行 `npm --prefix frontend run build:example:module-sdk` 即可生成 `output/modules/rust-sdk-counter/` 完整模块包，再将产物复制到 `MODULES_DIR` 下并启用。此示例是 Cargo workspace 成员，stdio 回归会随 `cargo test --workspace --locked` 执行。

业务模块源码放在主仓库之外的独立目录或独立仓库，模块作者自行构建和分发完整模块包。Firefly 仓库只保留通用宿主、[Rust MCP 适配库](../crates/module-sdk)、[UI SDK 与页面桥](../frontend/module-sdk)和无个人数据的协议示例；构建与测试不要求存在任何业务模块。将取得的模块包放入运行目录的 `modules/<模块 ID>/`，重新扫描并启用即可。

开发自己的功能时，从最小模板开始：修改清单中的 ID 和名称，实现工具名称、说明、参数 Schema 与业务处理。需要界面时再增加 MCP Apps 资源和 `pages` 声明，需要自动执行时增加 `jobs`。发布物是可独立运行的模块包及其依赖安装说明，安装者不需要修改 Firefly 的路由、菜单或工具注册代码。

## 接入现有 MCP 服务

也可以直接在模块目录中添加一个 `.toml` 文件：

```toml
id = "my-service"
name = "我的服务"

[mcp]
transport = "streamable_http"
url = "http://127.0.0.1:8080/mcp"
# 可选，值是服务进程环境中的变量名，不是实际凭据。
bearer_token_env = "MY_SERVICE_TOKEN"
```

HTTP 服务由安装者独立运行，Firefly 不启动它，也不会自动重发结果未知的工具调用。凭据和 HTTP 服务自身配置在服务端管理。HTTP 连接禁用重定向与过期会话自动重放。

本地工具使用 `stdio`：

```toml
id = "my-tool"
name = "我的工具"
call_timeout_secs = 60
# 可选：省略时允许该服务提供的所有工具；空数组不允许任何工具。
allowed_tools = ["lookup", "update"]

[mcp]
transport = "stdio"
command = "python3"
args = ["server.py"]

[mcp.env]
LANG = "C.UTF-8"

[mcp.env_from]
API_TOKEN = "MY_TOOL_API_TOKEN"
```

`command` 不经过 shell；不含 `/` 的命令通过 PATH 查找，相对路径相对于清单目录。`args` 原样传入，工作目录是清单所在目录。stdout 仅用于 MCP，stderr 写入模块数据目录的 `stderr.log`。除了 PATH、显式声明的变量和下述宿主变量，不继承 Firefly 服务的环境。

配置中的环境变量映射属于安装者授予模块的访问范围。本地模块是以服务运行身份执行的程序，进程分离不是操作系统权限沙箱；需要更严格隔离时，将模块作为独立用户或容器中的 HTTP 服务运行。

## 模块清单

完整目录中必须有 `module.toml`。清单版本 `manifest_version = 1`、`host_api = "1"`；其他版本拒绝加载。模块 ID 允许小写字母、数字和连字符，最多 64 字符。同 ID 的所有包均拒绝加载，避免目录顺序决定模块身份。

```toml
manifest_version = 1
host_api = "1"
id = "my-calendar"
name = "我的日历"
version = "0.1.0"
description = "自定义日期管理"
config_schema = "config.schema.json"

[mcp]
transport = "stdio"
command = "python3"
args = ["server.py"]

[[pages]]
id = "calendar"
title = "我的日历"
icon = "calendar" # 可选，使用宿主 UiIcon 名称；未知名称使用默认图标
open_tool = "list_dates"
arguments = {}

[[jobs]]
id = "daily-check"
tool = "check_reminders"
arguments = {}
cron = "0 9 * * *"
timezone = "Asia/Shanghai"
enabled_by_default = false
```

菜单点击 `open_tool`，把其结果发送给对应页面。工具在 `_meta.ui.resourceUri` 声明 `ui://` 资源，通过 MCP `resources/read` 返回 `text/html;profile=mcp-app` 的单文件 HTML；支持 `text` 或 base64 `blob`。页面通过 `ui/initialize` 与宿主握手，再发送 `ui/notifications/initialized`，随后接收工具输入与结果通知。

页面内的 `tools/call` 只能调用本模块允许的工具。`_meta.ui.visibility` 可以指定 `model`、`app` 或两者，省略时均可调用。模型看到的工具名带模块命名空间，过长或特殊名称会生成稳定的带摘要别名；页面仍使用 MCP 原始工具名。

页面运行在无同源权限的 iframe 中。HTML 响应单独设置 CSP，脚本和样式应内联打包；页面通过宿主消息桥访问工具。表单支持 JavaScript 提交事件，处理时应 `preventDefault()` 并调用工具；CSP 禁止原生表单跳转提交。任何消息必须来自当前 iframe，模块不能指定其他模块 ID 或任意代理 URL。参考 [MCP Apps 规范](https://modelcontextprotocol.io/extensions/apps/overview)。

## 状态提示

模块可以声明一个只读工具，为菜单和侧栏提供状态提示。业务判断留在模块中，宿主只负责轮询和显示；未安装或停用的模块不会被轮询。

```toml
[indicator]
tool = "get_status"
refresh_secs = 60 # 30–3600 秒
arguments = {}
```

工具必须声明 `annotations.readOnlyHint: true`、允许 `app` 调用，并包含在 `allowed_tools` 范围内（若配置了此项）。宿主通过鉴权接口 `GET /api/modules/{id}/indicator` 调用它，读取 MCP 结果的 `structuredContent.indicator`，例如：

```json
{
  "indicator": {
    "label": "自定义提醒",
    "icon": "activity",
    "tone": "info",
    "starts_at": "2026-10-13T13:30:00Z",
    "expires_at": "2026-10-13T16:00:00Z"
  }
}
```

无提醒时返回 `{"indicator": null}`。`label` 为 1–200 字符纯文本，`expires_at` 必填且最晚为当前时间之后 24 小时；`starts_at` 可省略，省略时立即生效。时间使用带时区的 ISO 8601。`tone` 支持 `info`、`warning`、`error`、`success`；图标使用宿主 `UiIcon` 名称。

菜单显示所属模块的提示，侧栏最多显示前三个。页面在前台时按声明间隔刷新；客户端每 15 秒检查起止时间，所以即使下一次请求失败也会自动到期消失。请求失败只保留尚未过期的既有提示；返回 null、停用模块、修改设置或退出登录会清除提示。这是界面内的提示，不是系统推送或定时任务。

## 设置与持久化

`config_schema` 是包内的 JSON Schema 文件，顶层为 object，设置字段使用内联 properties。管理页面直接编辑字符串、数字、布尔和枚举；对象、数组以 JSON 编辑。设置 Schema 暂不接受引用、组合条件及动态属性 Schema，以保证表单和凭据脱敏一致。工具参数支持本地 Schema 引用；工具和设置 Schema 都禁止外部引用。

stdio 模块收到以下宿主环境变量：

- `FIREFLY_MODULE_DATA_DIR`：此模块的持久化绝对路径。
- `FIREFLY_MODULE_CONFIG`：用户保存的配置 JSON。
- `FIREFLY_MODULE_TIMEZONE`：宿主的 `APP_TIMEZONE`。Rust SDK 的 `timezone()`、`today()`、`current_week_start_utc()`、`day_bounds()` 和时间格式化默认使用此时区；`day_bounds_in()` 可指定其他时区。明确以 Shanghai 命名的兼容函数仍使用上海时区。模块清单中的任务时区由模块独立声明。

设置保存时整体替换配置。Schema 中 `writeOnly: true` 或 `format: "password"` 的字段读回时为 null，提交 null 表示保留已有值；数组内的凭据按原索引保留，重排数组时需要重新填写凭据。凭据保存在本地 Firefly 数据库中；包中的示例和日志应避免写入真实凭据。

`MODULE_DATA_DIR` 默认 `storage/modules`。模块业务文件位于其 ID 子目录中，由模块负责数据结构和迁移。Firefly 数据库只保存启用状态、设置、任务进度与执行记录。备份需要同时包括 Firefly 数据库、模块数据目录、模块包及运行配置；停用或移除模块包不会删除已有数据。

## 重载与任务行为

“重新扫描”发现新包、移除已删除的包，并替换清单或设置变化的模块；未变化的模块保留连接。修改代码而不修改清单时，停用再启用以重启模块；发布升级应修改 `version`。

停用、替换或移除模块会撤销旧句柄并关闭连接，在途操作可能已经产生副作用。结果未知会明确报错，不自动重试写操作。模型任务取消也会关闭正在执行的模块连接。单个模块的请求串行执行，不影响其他模块。

定时任务使用五段 cron 和显式 IANA 时区，用户可以在模块管理页选择开启。调度器持久化原子领取记录；任务不重叠，不逐次补齐离线期间的每次错过执行。重启后正在执行的记录标记“结果未知”，不重跑。已有下一次执行时间若逾期，则合并执行一次并推进到未来。每个模块保留最近 100 条已结束记录，界面展示最近 30 条。

## 验证

```sh
cargo test --locked -p firefly-module-host
node --test --test-concurrency=1 frontend/src/composables/moduleBridge.test.js frontend/src/composables/useModules.test.js
npm --prefix frontend run test:module-sdk
npm --prefix frontend run check
```

Rust 回归会从临时目录安装协议示例、启动真实 Python MCP 进程，验证工具、页面资源、数据保留、停用撤销、HTTP 服务、任务去重和重启恢复。浏览器回归 `frontend/tests/e2e/test_modules_ui.py` 使用日期示例验证完整安装与页面交互；`frontend/tests/e2e/test_firefly_more_menu_ui.py` 验证模块菜单的触摸与键盘操作。两者都不依赖业务模块或模型凭据。

Rust SDK 示例的 stdio 回归随 workspace 测试执行；构建示例模块包后，`frontend/tests/e2e/test_rust_sdk_module_ui.py` 会验证实际宿主中的页面桥、配置变更、持久化及停用行为。命令见[示例说明](../examples/modules/rust-sdk-counter/README.md)。

运行浏览器回归前，需要本地安装 Python Playwright 及其 Chromium，然后执行：

```sh
npm --prefix frontend run build
cargo build --locked
python3 frontend/tests/e2e/test_modules_ui.py
```

## 模块与核心的维护边界

模块的构建脚本、业务测试和数据迁移由模块作者维护。升级主程序只替换宿主；已安装的模块包、模块数据和启用配置继续保留。模块升级按作者的说明单独完成。

## 其他宿主能力

- 工具参数上限 8 MiB，返回值上限 16 MiB，配置上限 64 KiB。
- `ui/clipboard` 写入文字 `{text}` 或 PNG data URL `{png}`；`ui/open-link` 只允许 HTTP/HTTPS URL。
- `ui/close` 关闭页面。声明 `appCapabilities.closeRequests: true` 的页面会先收到 `ui/notifications/close-request`，处理未保存确认后再发送 `ui/close`。不声明的旧模块保持原关闭行为。
- sandbox 增加 `allow-downloads`，仍不允许 same-origin 或网络访问。hostContext 提供 `origin` 供已公开链接使用，不提供认证凭据。
- 可选 `[public]` 声明 `prefix`（单段路径，不能覆盖 `/api`、`/auth`、`/health`、`/assets`）和 `tool`。宿主只调用被标记只读的 app 工具，输入 `{path}`，只接受 `{status:200,headers:{"content-disposition":"attachment;..."},base64:...}`。公开内容总是附件且不缓存。前缀冲突、模块停用、工具错误或非 200 结果均不提供内容。
