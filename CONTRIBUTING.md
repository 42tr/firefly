# 参与贡献

欢迎通过 Issue 说明问题或提出功能建议，通过 Pull Request 提交修改。漏洞请先阅读 [SECURITY.md](SECURITY.md)，不要在公开 Issue 中粘贴凭据或私有记录。

## 开发环境

安装 Rustup、Node.js 24、npm、C/C++ 编译器与 CMake。`rust-toolchain.toml` 固定工具链；`Cargo.lock` 和 `package-lock.json` 随依赖变更提交。首次启动和必填凭据见 [README](README.md)。测试使用独立数据库、存储和工作区，不连接个人生产服务。

```sh
npm ci
npm run build
cargo build --locked
```

前端源码与 Vite 配置在 `frontend/`，npm 命令在仓库根目录执行。`npm run dev` 提供开发页面，另起 Rust 后端使用真实接口。纯前端测试模拟接口，无需模型凭据。

Rust workspace 中，根 `firefly` crate 负责服务启动、HTTP API 和业务编排；`crates/workspace` 提供原生工作区准备与文件访问；`crates/module-host` 负责外部 MCP 模块的加载、连接和调度，由主程序传入目录、时区和数据库连接池。`crates/llm` 负责模型调用、流式解析、上下文压缩及预算计算，由主程序传入共享 HTTP 客户端和可选的模型限制缓存连接池。`crates/codex-transport` 提供 Codex CLI 传输，`crates/module-sdk` 供外部模块作者使用。数据库迁移统一保留在根目录的 `migrations_sqlite/`。

`crates/db` 提供 SQLite 连接、时间戳转换、自定义函数、忙等重试和在线备份。主程序的 `src/db.rs` 持有全局连接池、应用迁移和业务表结构测试，将连接池参数、时区和迁移对象传入数据库库。

`crates/claude-transport` 提供 Claude CLI 进程、事件传输和进程组中断，接收调用方构造的命令。主程序的 `src/claude.rs` 负责供应商和模型选择、CLI 参数、提示词及本轮临时 settings 文件的生命周期。

业务存储入口 `src/assistant_store.rs` 保留统一重导出，记录类型、会话操作、消息分页和附件访问分别位于 `src/assistant_store/{types,conversations,messages,attachments}.rs`，相关回归位于同目录的 `tests.rs`。消息与附件共用的事务写入辅助函数仅在存储模块内共享。

生成入口 `src/api/assistant/generation.rs` 保留执行与消息转换接口，Firefly、Codex、Claude 的流程分别位于同目录下 `generation/{firefly,codex,claude}.rs`，共享事件持久化与消息转换位于 `generation/events.rs`。事件契约、Codex 重试和 handoff 工作流回归分文件保留。

会话入口 `src/api/assistant/conversations.rs` 保留接口重导出和请求、响应类型；会话操作、执行设置校验、历史查询和展示转换分别位于 `conversations/{handlers,settings,history,presentation}.rs`。列表、过程摘要及 Codex/Claude 设置回归各自保留独立测试文件。

可以单独运行新库的测试：

```sh
cargo test --locked -p firefly-workspace -p firefly-module-host -p firefly-llm -p firefly-db -p firefly-claude-transport
```

## 验证修改

按改动范围运行检查，完整基础检查如下：

```sh
npm run check
npm run test:module-sdk
npm run audit:deps
cargo test --workspace --locked -- --test-threads=4
cargo clippy --workspace --locked --all-targets -- -D clippy::correctness
cargo build --locked
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s src -p '*workspace_test.py'
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s src/api -p '*_test.py'
```

独立 Python 回归调用 `target/debug/firefly` 的原生工作区和文件服务，运行前先构建二进制；可用 `FIREFLY_TEST_BINARY` 指向其他构建产物。复制期间文件消失、来源变化与 I/O 故障的注入测试在 Rust 中运行。Python 是这些测试的驱动器，服务运行时不依赖 Python。

依赖更新还需查看 RustSec / OSV 公告和依赖链，区分安全漏洞与停止维护通知，不将未解决漏洞直接加入忽略列表。更新后可在本地重新生成第三方许可证清单查看变化（生成文件不入库，发布流程自动生成）：

```sh
python3 scripts/generate_third_party_licenses.py
```

不稳定测试应修正隔离、时钟或异步条件，保留行为断言，避免以增加等待、重试或删除断言掩盖失败。任务轮询测试使用独立时钟，避免替换 Vite 加载器使用的全局计时器。

## 可选浏览器测试

Python 3.11 或更新版本：

```sh
python3 -m venv .venv
. .venv/bin/activate
python3 -m pip install -r requirements-dev.txt
python3 -m playwright install chromium
# Linux 如缺少浏览器系统库，按 Playwright 提示安装对应依赖。
```

一个终端运行开发页面，另一个终端执行对应脚本，通过 `FIREFLY_TEST_URL` 指定地址：

```sh
npm run dev -- --host 127.0.0.1 --port 5199
```

```sh
FIREFLY_TEST_URL=http://127.0.0.1:5199 python3 scripts/test_local_login_ui.py
FIREFLY_TEST_URL=http://127.0.0.1:5199 python3 scripts/test_conversation_archive_ui.py
FIREFLY_TEST_URL=http://127.0.0.1:5199 python3 scripts/test_workflow_ui.py
FIREFLY_TEST_URL=http://127.0.0.1:5199 python3 scripts/test_streaming_ui.py
python3 scripts/test_modules_ui.py
```

模块宿主测试启动临时后端，运行前先构建前端及 `cargo build --locked`，使用仓库内的协议示例。截图写入忽略的 `output/`。

已安装 Claude CLI 时，还可运行原生 settings 协议回归；它使用隔离配置、虚构凭据和本地模拟 API，验证平台覆盖、凭据助手、推理级别及恢复会话，不调用真实模型：

```sh
python3 -m unittest scripts.test_claude_settings_protocol
```

## 提交约定

- 说明触发场景、最终行为和验证结果，界面变更提供桌面与移动端结果。
- 保持改动聚焦，尊重 API、配置脱敏与模块权限边界。
- 当前数据库从 `0001_firefly_baseline.sql` 初始化，不支持旧开发版本升级；后续结构变更新增迁移，保持已发布迁移名称、内容及校验和不变。
- 不提交 `.env`、数据库、工作区、日志、截图或真实姓名、日期、账号、服务地址；示例使用虚构内容和示例域名。
- 图标修改沿用许可声明，界面使用系统字体；增加依赖或资源时更新第三方声明。

工作流只在版本 tag 推送时运行，验证包含前端检查、模块 SDK 测试、Rust 测试、Clippy correctness 检查及独立 Python 工作区和文件服务回归。PR 提交前由贡献者执行相关本地检查，维护者发布前完成整套验证、更新版本，再创建 `v*` tag。提交贡献即表示允许其按本项目 MIT 许可证分发，第三方资源仍按原许可证分发。
