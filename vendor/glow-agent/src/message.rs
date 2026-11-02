use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    #[default]
    Auto,
    Low,
    High,
    Original,
}

impl ImageDetail {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::High => "high",
            Self::Original => "original",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    FileId { file_id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        detail: ImageDetail,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        result: Value,
        is_error: bool,
    },
    ProviderData {
        provider: String,
        data: Value,
    },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        Self::image_url_with_detail(url, ImageDetail::Auto)
    }

    pub fn image_url_with_detail(url: impl Into<String>, detail: ImageDetail) -> Self {
        Self::Image {
            source: ImageSource::Url { url: url.into() },
            detail,
        }
    }

    pub fn image_file(file_id: impl Into<String>) -> Self {
        Self::image_file_with_detail(file_id, ImageDetail::Auto)
    }

    pub fn image_file_with_detail(file_id: impl Into<String>, detail: ImageDetail) -> Self {
        Self::Image {
            source: ImageSource::FileId {
                file_id: file_id.into(),
            },
            detail,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn new(role: Role, content: Vec<Content>) -> Self {
        Self { role, content }
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![Content::Text { text: text.into() }],
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn user_content(content: Vec<Content>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|content| match content {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

pub trait IntoPrompt {
    fn into_prompt(self) -> Message;
}

impl IntoPrompt for String {
    fn into_prompt(self) -> Message {
        Message::user(self)
    }
}

impl IntoPrompt for &str {
    fn into_prompt(self) -> Message {
        Message::user(self)
    }
}

impl IntoPrompt for &String {
    fn into_prompt(self) -> Message {
        Message::user(self)
    }
}

impl IntoPrompt for Content {
    fn into_prompt(self) -> Message {
        Message::user_content(vec![self])
    }
}

impl IntoPrompt for Vec<Content> {
    fn into_prompt(self) -> Message {
        Message::user_content(self)
    }
}

impl<const N: usize> IntoPrompt for [Content; N] {
    fn into_prompt(self) -> Message {
        Message::user_content(self.into())
    }
}

impl IntoPrompt for Message {
    fn into_prompt(self) -> Message {
        self
    }
}
