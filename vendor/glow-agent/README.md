# Glow

`glow-agent` 是一个轻量的 Rust Agent 运行时。它负责在模型、工具和子 Agent 之间驱动多轮对话，并提供流式事件、工具调用策略、运行限额、取消以及图像输入等基础能力。

目前内置两个 OpenAI 模型适配器：`OpenAIModel` 基于 Responses API，`OpenAIChatModel` 基于 Chat Completions API；同时也可以通过实现 `Model` trait 接入其他模型。

## 功能

- 使用 `Agent` 构建并运行单 Agent 工作流
- 通过 `#[tool]` 将异步 Rust 函数声明为模型工具，并自动生成 JSON Schema
- 支持工具调用、错误回传和调用前授权策略
- 支持将任务 handoff 给具名子 Agent
- 支持文本增量及运行生命周期事件
- 支持 URL、data URL 和 OpenAI file ID 形式的图像输入
- 支持最大工具调用数、超时和主动取消
- 通过 `Model`、`Tool`、`Policy` 和 `EventSink` trait 扩展运行时
- `OpenAIModel`（Responses API）内置每轮输入预算、工具参数校验与修复重生成、流式空闲超时与请求诊断日志
- 可选 `mcp` / `skills` feature：每次请求显式传配置，支持运行时替换，无目录扫描；见 [使用说明](docs/extensions.md)
- 可选 `limits` feature：`ModelBudget` 按模型服务元数据自动发现上下文窗口（含 SQLite 持久缓存，表可自建），`ContextModel` 提供滚动摘要式上下文压缩，`OpenAIModel::with_context_compaction()` 一键启用

## 环境要求

- Rust 2024 edition 对应的工具链
- 可用的 OpenAI API Key，或兼容 OpenAI Responses API / Chat Completions API 的服务

配置环境变量：

```bash
export OPENAI_API_KEY="your-api-key"
# 可选，responses 或 chat/completions，默认值为 chat
export OPENAI_TYPE="chat"
# 可选，Responses API 默认值为 gpt-5.6，Chat Completions API 默认值为 gpt-4o
export OPENAI_MODEL="gpt-4o"
# 可选，默认值为 https://api.openai.com/v1
export OPENAI_BASE_URL="https://api.openai.com/v1"
```

`OPENAI_BASE_URL` 应指向 API 的 `/v1` 根路径。`OpenAIModel` 会向其下的 `/responses` 发起请求，`OpenAIChatModel` 会向其下的 `/chat/completions` 发起请求。

如果希望通过单一入口根据 `OPENAI_TYPE` 自动选择接口，可以使用 `glow_agent::openai_from_env()`：

```rust
use glow_agent::{openai_from_env, Agent};
use std::sync::Arc;

let model = openai_from_env()?;
let agent = Agent::new(model);
```

## 快速开始

克隆仓库后，可以直接运行内置示例：

```bash
cargo run --example calculator
```

在其他项目中使用当前 Git 版本：

```toml
[dependencies]
glow-agent = { git = "https://github.com/42tr/g.git" }
futures-util = "0.3"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt"] }
```

发布到 crates.io 后，可改用版本依赖：

```toml
[dependencies]
glow-agent = "0.3.4"
```

本仓库包含 `glow-agent` 运行时和 `glow-agent-macros` 过程宏两个包。
应用只需依赖运行时，Cargo 会自动下载配套宏包；`tool` 宏从
`glow_agent::tool` 统一导出。

从原来的 `g` 包迁移时，可保留依赖别名，让现有的 `g::Agent`、`g::tool`
调用继续使用：

```toml
[dependencies]
g = { package = "glow-agent", version = "0.3.4" }
```

Git 依赖或本地 `path` 依赖同样可以保留 `g` 别名，但也需要指定
`package = "glow-agent"`。

下面的例子声明了一个加法工具，并以流式方式输出模型回复：

```rust
use std::{
    io::{self, Write},
    sync::Arc,
};

use futures_util::StreamExt;
use glow_agent::{tool, Agent, OpenAIModel, RunEvent, ToolCallError};
use serde_json::{json, Value};

#[tool(name = "add", description = "Add two integers")]
async fn add(left: i64, right: i64) -> Result<Value, ToolCallError> {
    Ok(json!({ "sum": left + right }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = Agent::new(Arc::new(OpenAIModel::from_env()?))
        .tools([add])
        .instruction("Always use the add tool for addition questions.");

    let mut events = agent.stream_run("What is 20 + 22?");
    while let Some(event) = events.next().await {
        match event? {
            RunEvent::TextDelta { text, .. } => {
                print!("{text}");
                io::stdout().flush()?;
            }
            RunEvent::Completed { .. } => println!(),
            _ => {}
        }
    }

    Ok(())
}
```

不需要增量输出时，使用 `run`：

```rust
let output = agent.run("What is 20 + 22?").await?;

println!("{}", output.final_text);
println!("turns: {}, tool calls: {}", output.turns, output.tool_calls);
println!(
    "tokens: {} input / {} output",
    output.usage.input_tokens,
    output.usage.output_tokens
);
```

## 工具

`#[tool]` 仅支持异步自由函数。函数参数会被转换为工具的 JSON Schema 和调用参数；`Option<T>` 参数不会出现在 `required` 列表中。函数必须返回 `Result<T, E>`，其中输出需要可序列化，错误需要实现 `Display`。

```rust
#[tool(name = "lookup", description = "Look up recent market values")]
async fn lookup(
    market: String,
    days: Option<u32>,
) -> Result<Value, ToolCallError> {
    Ok(json!({ "market": market, "days": days }))
}
```

未指定 `description` 时使用函数的文档注释。参数可以是任意实现了 `JsonSchema` 的类型，嵌套结构体会统一放在 schema 根部的 `$defs` 中。类型为 `ToolContext` 的参数（最多一个）不会出现在 schema 中，运行时会注入本次运行的 `run_id` 和 `CancellationToken`：

```rust
/// Look up recent market values.
#[tool]
async fn lookup(query: Query, context: ToolContext) -> Result<Value, ToolCallError> {
    if context.cancellation_token.is_cancelled() { /* ... */ }
    Ok(json!({ "market": query.market }))
}
```

需要完整控制 schema 或行为元数据时，可以直接实现 `Tool` trait。

工具执行失败时，错误会作为带有 `is_error: true` 的工具结果返回给模型，由模型决定如何继续。

同一轮中连续的、`ToolBehavior::parallel_safe` 为 `true` 的工具会并发执行，结果仍按调用顺序写回；其他工具和 handoff 按顺序执行。

## Handoff

父 Agent 可以把任务委派给具名子 Agent。子 Agent 必须设置非空且不重复的 `name`；运行时会为其生成 `handoff_to_<name>` 工具。

```rust
let model = Arc::new(OpenAIModel::from_env()?);

let math_agent = Agent::new(model.clone())
    .name("math")
    .description("Solve arithmetic questions")
    .tools([add])
    .instruction("Use the available tools and return a concise answer.");

let agent = Agent::new(model)
    .handoff([math_agent])
    .instruction("Hand off arithmetic questions to the math agent.");

let output = agent.run("What is 20 + 22?").await?;
println!("{}", output.final_text);
```

Handoff 会继承父运行的取消信号和事件接收器，并复用父运行已发现的 MCP 工具。子 Agent 的 token 用量会累加到父运行结果中。Policy 收到的 handoff 来源是 `ToolOrigin::Handoff { agent }`。

## 图像输入

将文本和图像内容组合后直接传给 `run` 或 `stream_run`：

```rust
use glow_agent::{Content, ImageDetail};

let prompt = [
    Content::text("Describe this image in one sentence."),
    Content::image_url_with_detail(
        "https://example.com/image.jpg",
        ImageDetail::Low,
    ),
];

let output = agent.run(prompt).await?;
```

可用的图像构造方法包括 `Content::image_url`、`Content::image_url_with_detail`、`Content::image_file` 和 `Content::image_file_with_detail`。图像目前只支持出现在用户消息中。

## 策略、事件和运行限制

默认策略是 `AllowAll`。可以实现 `Policy` 并通过 `with_policy` 在每次工具执行前检查工具及其参数：

```rust
let agent = Agent::new(model).with_policy(Arc::new(MyPolicy));
```

策略拒绝默认直接结束本次运行（`AgentError::Policy`）。设置 `on_policy_denial(PolicyDenial::ReportToModel)` 后，拒绝会作为错误工具结果（`{"error": {"code": "policy_denied", ...}}`）返回给模型。

`stream_run` 会返回以下 `RunEvent`，除 `Finished` 外都带有产生它的运行的 `run_id`，可以据此区分父运行和 handoff 子运行：

- `Started` / `Completed`
- `ModelStarted` / `ModelCompleted` / `ModelRetry`
- `TextDelta`
- `ToolStarted` / `ToolCompleted`
- `HandoffStarted` / `HandoffCompleted`
- `Finished { output }`：运行成功时流的最后一项，携带完整的 `RunOutput`

也可以实现 `EventSink`，再通过 `with_event_sink` 将事件发送到日志、指标或审计系统。

不设模型轮数上限；默认限制为 32 次工具调用和 120 秒超时：

```rust
use std::time::Duration;
use glow_agent::RunLimits;

let agent = Agent::new(model).with_limits(RunLimits {
    max_tool_calls: 16,
    timeout: Duration::from_secs(60),
});
```

标记为 retryable 的模型错误（网络错误、429、5xx）默认以指数退避重试 2 次；已经输出过文本的请求不会重试。可以通过 `with_retry_policy(RetryPolicy { .. })` 调整，或使用 `RetryPolicy::none()` 关闭。OpenAI provider 默认的 HTTP 连接超时为 30 秒、读取间隔超时为 300 秒，可以用 `with_http_client` 替换客户端。

如需主动取消，使用 `Runtime::run` 和带 `CancellationToken` 的 `RunRequest`。丢弃 `stream_run` 返回的事件流后，运行也会在下一次发送事件时收到取消信号。

## 示例

```bash
# 工具调用与流式输出（Responses API）
cargo run --example calculator

# 工具调用与流式输出（Chat Completions API）
cargo run --example chat_completions

# 图像理解
cargo run --example vision

# Agent handoff
cargo run --example handoff
```

设置 `RUST_LOG` 可以查看运行日志：

```bash
RUST_LOG=glow_agent=debug cargo run --example calculator
```

## 开发

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets
```

## 发布

两个包保持相同版本号，运行时通过精确版本依赖配套宏包。发布前在两个
`Cargo.toml` 中更新版本号和宏依赖版本，并运行测试：

```bash
cargo fmt --all --check
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked
```

使用支持 workspace 发布的 Cargo；本次验证工具链为 Cargo 1.97.1。
脚本默认只做打包和编译预检，不上传，Cargo 会按依赖顺序处理两个包：

```bash
sh scripts/publish.sh
# 审阅尚未提交的本地改动时
sh scripts/publish.sh --allow-dirty
```

确认预检通过、发布改动已提交，并配置好 crates.io 发布凭据后，显式执行：

```bash
sh scripts/publish.sh --publish
```

脚本指定 `--registry crates-io`，避免本地下载镜像影响发布目标。

首次同时预检两个尚未发布的包时，配置了 `source.crates-io.replace-with`
的下载镜像可能使 Cargo 无法找到刚打包的宏包。此时使用独立 Cargo
缓存目录，直接访问 crates.io，不必修改全局配置：

```bash
CARGO_HOME="$PWD/target/publish-cargo-home" sh scripts/publish.sh --allow-dirty
```

正式发布也使用该目录时，需要先在同一 `CARGO_HOME` 下配置发布凭据。

## 许可证

本项目采用 `MIT OR Apache-2.0` 双许可证，由使用者任选其一。
完整文本见 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。
