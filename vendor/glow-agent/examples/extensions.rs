//! Run with: cargo run --example extensions --features mcp,skills
//! The application supplies JSON in G_EXTENSIONS_JSON; the library never reads it.
use glow_agent::{Agent, ExtensionConfig, Message, RunRequest, Runtime, mcp::McpManager};
use std::sync::Arc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config: ExtensionConfig = serde_json::from_str(
        &std::env::var("G_EXTENSIONS_JSON").unwrap_or_else(|_| {
            r#"{"skills":[{"name":"task-analysis","description":"Analyze a task","instructions":"Separate observed facts from missing evidence."}]}"#.into()
        }),
    )?;
    config.validate()?;
    let agent = Agent::new(glow_agent::openai_from_env()?)
        .instruction("Use the configured skills when relevant.");
    let manager = Arc::new(McpManager::anonymous());
    let runtime = Runtime::new().with_mcp_manager(manager.clone());
    let request = RunRequest::new(vec![Message::user("Explain how you would analyze a task.")])
        .with_extensions(Arc::new(config));
    let result = runtime.run(&agent, request).await;
    let closed = manager.close().await;
    let output = result?;
    closed?;
    println!("{}", output.final_text);
    // Persist output.conversation_messages; provide current configuration on the next run.
    Ok(())
}
