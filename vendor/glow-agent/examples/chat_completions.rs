use std::{
    io::{self, Write},
    sync::Arc,
};

use futures_util::StreamExt;
use glow_agent::{Agent, OpenAIChatModel, RunEvent, ToolCallError, tool};
use serde_json::{Value, json};

#[tool(name = "add", description = "Add two integers")]
async fn add(left: i64, right: i64) -> Result<Value, ToolCallError> {
    Ok(json!({ "sum": left + right }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let model = Arc::new(OpenAIChatModel::from_env()?);
    let agent = Agent::new(model)
        .tools([add])
        .instruction("Always use the add tool to answer arithmetic addition questions.");

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
