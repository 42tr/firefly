# SQLite 日常操作

`DATABASE_URL` 可省略，默认值为 `sqlite://firefly.sqlite3`，相对于进程工作目录解析。也可显式设置 `sqlite:///absolute/path/to/firefly.sqlite3`。使用持久化本地磁盘，运行账户需要数据库及父目录的读写权限；启动前确认当前目录和配置指向同一安装实例。

## 构建和初始化

直接使用 `cargo` 构建或测试。SQLx 0.9 与备份工具共享 `libsqlite3-sys 0.37.0` 内置的 SQLite 3.51.3，由 Cargo 编译并静态链接，不依赖系统 SQLite 或额外下载源码的脚本。依赖版本由 `Cargo.toml` 和 `Cargo.lock` 固定；运行时仍检查最低版本 3.51.3。构建需要 Rust 1.94 或更新版本和 C 编译器。

```sh
cargo test --workspace --locked
cargo build --locked
```

数据库文件不存在时，启动会自动以 0600 权限创建并执行全部基线迁移，父目录须已存在且可写。已存在的文件不会被自动接管：缺少 `_sqlx_migrations` 表时启动拒绝继续。也可用显式初始化预先建库，要求绝对路径且目标文件必须不存在：

```sh
./target/debug/firefly --init-db sqlite:///absolute/new-install/firefly.sqlite3
```

由当前基线初始化的数据库无需重复初始化。启动会使用 WAL、`synchronous=NORMAL`（不会损坏库，断电可能丢最后几次提交，换来提交时不再 fsync、写锁占用更短）、外键检查和 30 秒写锁等待；助手生成路径的写入超时后经 `db::retry_busy` 退避重试，只包裹可安全重跑的单条语句或整个事务；读写事务使用 `BEGIN IMMEDIATE`，事务内不要等待外部网络请求。

服务每天按 `APP_TIMEZONE` 的 04:20 执行一次 `PRAGMA optimize`（`db::optimize`），默认时区为 `Asia/Shanghai`：按 SQLite 3.18+ 的规则只对统计变化明显的表补跑 ANALYZE，避免无条件全库 ANALYZE 的开销；写 sqlite_stat1 时同样走 `db::retry_busy`。该任务属于内部维护，不受可选模块任务开关影响。

## 新库结构

`migrations_sqlite/0001_firefly_baseline.sql` 是建库基线，直接创建对话、消息、生成任务、附件、分支、工作区提交、工具任务与事件、工具详情、定时任务、模型缓存、模块配置与执行记录、Agent 配置共 15 张业务表。后续迁移按顺序增量执行；`0002_claude_platform_and_effort.sql` 为已有会话添加 Claude 平台和推理级别字段，保留会话、历史及旧任务配置。已停用的记忆、待办、旧周报及其他内置业务表不再创建；独立模块的数据由模块自行管理。

`db::MIGRATOR` 在初始化时执行该 SQL，`_sqlx_migrations` 记录版本和校验和。当前基线不兼容旧开发版本数据库，不提供旧库升级或历史数据回填。使用旧库的实例须先备份，再通过 `--init-db` 在新路径建库，停服后同时切换新二进制和 `DATABASE_URL`。旧库保留作备份；模型及模块配置需要重新设置或单独导入。不要删除旧库迁移记录来强行启动。

后续基于此基线的结构变更应新增迁移，避免修改已发布基线的校验和。部署回滚时二进制与数据库版本须匹配。

## 在线备份

```sh
./firefly --backup-db \
  ./firefly.sqlite3 \
  /absolute/backups/firefly-backup.sqlite3
```

工具使用 SQLite 在线备份 API，包含已提交的 WAL 数据，检查完整性和外键，生成权限 0600 的独立文件，拒绝覆盖已有目标。不要直接复制正在使用的 `.sqlite3` 主文件作为完整备份。

备份命令复用二进制内静态链接的 SQLite，无需 Python、编译器、运行配置或启动服务。两个参数均为文件路径，目标必须是新文件。备份应复制到异机，并在独立目录定期验证恢复。附件、素材和工作区需另外备份。

恢复时由独立运维会话停写，先保留当前数据库，再替换为已验证的备份；数据库文件与对应 WAL/SHM 必须来自同一状态。不要把备份直接覆盖到运行中的数据库上。
