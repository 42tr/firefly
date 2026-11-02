use std::sync::Arc;

use glow_agent::{Agent, OpenAIModel, ToolCallError, tool};
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

    let model = Arc::new(OpenAIModel::from_env()?);
    let math_agent = Agent::new(model.clone())
        .name("math")
        .description("Solve arithmetic questions with the available tools")
        .tools([add])
        .instruction("Use the add tool for addition and return a concise answer.");

    let agent = Agent::new(model)
        .handoff([math_agent])
        .instruction("Hand off arithmetic questions to the math agent.");

    let output = agent.run("What is 20 + 22?").await?;
    println!("{}", output.final_text);
    Ok(())
}
