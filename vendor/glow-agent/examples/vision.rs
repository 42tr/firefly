use std::{
    io::{self, Write},
    sync::Arc,
};

use futures_util::StreamExt;
use glow_agent::{Agent, Content, ImageDetail, OpenAIModel, RunEvent};

const IMAGE_URL: &str =
    "https://api.nga.gov/iiif/a2e6da57-3cd1-4235-b20e-95dcaefed6c8/full/!800,800/0/default.jpg";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let agent = Agent::new(Arc::new(OpenAIModel::from_env()?));
    let prompt = [
        Content::text("Describe this image in one concise sentence."),
        Content::image_url_with_detail(IMAGE_URL, ImageDetail::Low),
    ];

    let mut events = agent.stream_run(prompt);
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
