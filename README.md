# Firefly 流萤

面向个人部署的 AI 工作应用，使用 Rust、Axum、SQLite 和 Vue 3。提供流萤对话、Codex 与 Claude 工作区、附件和可配置 Agent，支持安装独立 MCP 扩展模块。

这是单用户应用。登录者可以使用获授权的 Agent 工具及服务账户的 CLI 和文件权限，适合可信使用者自行部署。生产运行只需要一个包含前端的二进制，不依赖其他应用的目录或数据库。界面使用系统字体，没有外部字体请求或字体文件。

## 首次安装

### 使用 Release

从 [GitHub Releases](https://github.com/42tr/firefly/releases) 下载对应平台的压缩包与 `SHA256SUMS`：`firefly-linux-x86_64.tar.gz`、`firefly-linux-arm64.tar.gz` 或 `firefly-macos-arm64.tar.gz`。Linux 使用 musl 静态构建。将下载文件放在同一目录，Linux 运行 `sha256sum --ignore-missing -c SHA256SUMS`；macOS 运行 `shasum -a 256 firefly-macos-arm64.tar.gz`，对照校验和文件中的对应行。

校验后解压到一个持久化目录，例如 `~/firefly`。以下以 Linux x86_64 为例，其他平台更换压缩包文件名：

```sh
mkdir -p ~/firefly
tar -xzf firefly-linux-x86_64.tar.gz -C ~/firefly
cd ~/firefly
cp .env.example .env
chmod 600 .env
```

**初始化前编辑 `.env`**，替换 `AUTH_PASSWORD`（至少 9 个字符，使用强密码）和 `JWT_SECRET`（至少 32 字节）。可用 `openssl rand -hex 32` 分别生成两个独立随机值。本机 HTTP 访问保留 `COOKIE_SECURE=false`；HTTPS 部署设置 `COOKIE_SECURE=true`。示例凭据会被拒绝，不能直接启动。

```sh
./firefly
```

访问 `http://localhost:3003/`，输入本站访问密码；`/health` 为健康检查。数据库文件不存在时启动会自动创建（0600 权限）并执行基线迁移，也可用 `./firefly --init-db "sqlite://$(pwd)/firefly.sqlite3"` 预先显式建库。始终从同一个安装目录启动，以确保配置、数据库、附件和工作区指向同一实例。

### 从源码安装

需要 [Rustup](https://rustup.rs/)（仓库固定工具链）、Node.js 24、npm、C/C++ 编译器与 CMake。Linux 可先安装 `build-essential cmake pkg-config`，macOS 安装 Xcode Command Line Tools 和 CMake。SQLite 由 Cargo 编译并静态链接，不要求系统安装 SQLite。工作区文件浏览、下载打包、临时图片读取和并行副本准备均由 Rust 二进制实现，运行服务无需 Python；并行副本需要 Git。测试、维护脚本及可选的 Python 示例模块仍使用 Python。

```sh
git clone https://github.com/42tr/firefly.git
cd firefly
cp .env.example .env
chmod 600 .env
# 按上一节替换 AUTH_PASSWORD 和 JWT_SECRET，再继续。
npm ci
npm run build
cargo build --release --locked
./target/release/firefly
```

前端依赖由根目录的 `package.json` 管理，产物在 `frontend/dist/`；Rust 编译前先构建前端。开发时运行 `npm run dev`，Vite 将 API、登录、WebSocket 和下载代理到本机 3003 端口，通过 `.env` 指定独立的数据库、存储和工作区。

## 配置 Agent 与扩展

左下角设置菜单统一提供“Agent 配置”“扩展模块”和“退出登录”。在 Agent 配置中填写支持 Responses API 的模型服务、模型与密钥，分配工具后保存。`OPENAI_*` 仅用于首次初始化，以后以页面保存的配置为准。无需模型密钥即可启动和进入配置页面。

Codex 和 Claude Code 通过运行账户的 `codex`、`claude` CLI 执行，可按需安装并配置登录或 API 凭据。CLI 登录属于该账户，不随 Firefly 二进制分发。Claude 的多平台、模型和推理级别配置见[对话文档](docs/assistant.md)。

运行配置项说明见 [.env.example](.env.example)。

将独立模块包放入 `MODULES_DIR`（默认 `modules/`），在扩展模块页重新扫描、配置并启用；工具权限和定时任务分别开启。本仓库提供 [Rust / UI SDK](https://github.com/42tr/firefly/tree/main/crates/module-sdk)与[协议示例](examples/modules)。安装、开发协议及任务说明见[模块文档](docs/modules.md)。

要运行使用 SDK 的完整示例，在源码目录执行 `npm run build:example:module-sdk`，再安装生成的计数器模块包。构建、安装和代码对应关系见 [Rust / UI SDK 示例](examples/modules/rust-sdk-counter/README.md)。

## 数据、升级与备份

默认布局相对于启动目录，可通过 `.env` 覆盖：

```text
.env                              # 本站配置与私有凭据
firefly.sqlite3                   # 对话、配置与任务
storage/assistant_uploads/         # 附件
storage/modules/                  # 模块数据
workspace/                        # Agent 工作目录
modules/                          # 独立模块包
```

模型及模块密钥存储在本地数据库，接口读回脱敏；数据库备份仍包含密钥，必须妥善保管。服务默认监听 `0.0.0.0:3003`，仅本机使用时将 `BIND_ADDRESS` 改为 `127.0.0.1:3003`。反向代理提供 HTTPS 时设置 `COOKIE_SECURE=true`。登录和 WebSocket 使用密码与 JWT 会话认证，不检查 Origin 白名单；旧配置中的 `SITE_URL`、`SITE_URL_ALIASES` 可删除。工作区不是隔离不可信使用者的沙箱。

`CONVERSATION_RETENTION_DAYS` 配置对话未更新的保留天数，默认 30，设置为 0 关闭自动清理。启用时每日维护任务删除达到期限的对话、关联消息、记录及附件；查看不会延长保留期，归档和置顶同样遵循该配置。`APP_TIMEZONE` 配置核心调度、周统计、日志与页面时间显示的 IANA 时区，默认 `Asia/Shanghai`。修改后重启；已有定时任务的下次运行时间会在时区变化时重新计算，普通重启保留原触发时间。模块任务继续使用各自清单中的显式时区。

`ASSISTANT_MAX_ATTACHMENT_MB` 配置单个上传附件的大小上限，默认 `100`，单位 MB（1 MB = 1024 × 1024 字节），必须为正整数，普通文件和视频使用相同限制。前端接受文件后由服务端校验大小，修改 `.env` 后重启服务生效。整次上传请求默认最多 256MB；配置较大附件上限时，请求上限自动提升至单附件上限加 1MB，为表单元数据预留空间。

```sh
./firefly --backup-db ./firefly.sqlite3 /absolute/backups/firefly-backup.sqlite3
```

备份工具包括已提交的 WAL 数据，校验完整性并拒绝覆盖目标。另行备份 `.env`、`storage/`、`workspace/` 和模块包。当前版本采用全新建库基线，不兼容旧开发版本数据库；旧实例需备份后在新路径初始化数据库，停服后同时切换新二进制和数据库配置。由当前基线创建的数据库可直接重启使用。恢复及兼容说明见 [SQLite 操作手册](docs/sqlite.md)。

## 开发与发布

开发环境、检查命令、浏览器测试及提交约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。对话接口、后台任务、恢复与预算见 [Agent 文档](docs/assistant.md)。安全问题报告见 [SECURITY.md](SECURITY.md)。

GitHub Actions **仅在推送 `v*` tag 时**验证、构建并发布 Release。分支推送、PR 和手动操作不触发，不部署服务器。三个平台的压缩包包含前端、配置示例、文档、许可证及第三方声明，Release 附带 SHA-256 校验和。发布步骤：

```sh
# 先按 CONTRIBUTING.md 完成本地检查、提交代码并推送 main。
git tag -a v0.1.4 -m 'Firefly v0.1.4'
git push origin v0.1.4
```

tag 必须与 `Cargo.toml`、`package.json` 版本一致。重新发布使用新 tag，避免覆盖已有发布。使用仓库提供的 `GITHUB_TOKEN`，不需要服务器地址、密码或 SSH secrets。

## 许可证

项目采用 [MIT](LICENSE) 许可证。第三方资源与依赖保留各自许可证，见[第三方声明](THIRD_PARTY_NOTICES.md)；完整的第三方许可证清单由发布流程生成并随二进制 Release 提供，也可运行 `python3 scripts/generate_third_party_licenses.py` 在本地生成。
