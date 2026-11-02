# Firefly Claude Transport

Claude CLI 进程管理、逐行 JSON 事件读取、会话及文本增量提取和进程组中断。丢弃 turn 时也会清理本地工具子进程。

调用方构造 `tokio::process::Command`，传入可执行文件、参数、工作目录和环境。`ClaudeTurn::spawn(command)` 用于关闭输入的普通命令；Claude 会话使用 `spawn_streaming(command)` 和 `send()` 保持 JSON 行输入开启。库设置 stdin/stdout/stderr 和进程组，在独立任务中读取 stdout；`next_event()` 跳过空行，检查 UTF-8、JSON 和退出状态，`interrupt()` 可重复调用。`Activity` 核对会话空闲、主线程结果和后台任务列表，单个 `result` 不代表全部工作完成。

主程序的 `src/claude.rs` 保留供应商选择、模型与推理级别校验、CLI 参数和提示词；`src/claude/sessions.rs` 按用户和对话持有 CLI 及其临时 settings 文件。库不读取 Firefly 全局配置，也不依赖数据库或 HTTP API。

```sh
cargo test --locked -p firefly-claude-transport
```

传输回归使用本地 Python 模拟进程，无需安装 Claude CLI 或配置模型凭据。覆盖命令注入、事件解析、错误退出、stderr 排空，以及中断和丢弃 turn 后的子进程清理。供应商配置及临时文件生命周期由主程序测试覆盖。
