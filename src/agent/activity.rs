use anyhow::Result;
use async_trait::async_trait;
use g::{Tool,ToolBehavior,ToolError,ToolSpec};
use serde_json::{json,Value};
use crate::{assistant_store,utils};
const MAX_USER_MESSAGE_BYTES:usize=500;
const MAX_LAST_REPLY_BYTES:usize=2000;
const MAX_USER_MESSAGES:usize=50;
/// 查询本周工作记录的工具。`g::tool` 宏无法捕获上下文，
/// 这里手动实现 `Tool` 以绑定调用方用户。
#[derive(Clone)]
pub struct FetchActivityRecordsTool {
    pub user_id: String,
}

#[async_trait]
impl Tool for FetchActivityRecordsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fetch_activity_records".to_string(),
            description: "查询当前自然周（周一起）内有活动的 Codex 和 Claude 对话，\
                每个对话返回用户发出的消息和 agent 的最后一次回复。"
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
            behavior: ToolBehavior { read_only: true, idempotent: true, parallel_safe: true },
        }
    }

    async fn call(&self, _context: g::ToolContext, _input: Value) -> Result<Value, ToolError> {
        let timezone = crate::config::timezone();
        let week_start = utils::current_week_start_utc();
        let records = assistant_store::fetch_weekly_agent_work(&self.user_id, week_start)
            .await
            .map_err(|err| ToolError::new(format!("{:#}", err)))?;

        let conversations = records
            .into_iter()
            .map(|record| {
                let user_message_count = record.user_messages.len();
                let user_messages = record
                    .user_messages
                    .iter()
                    .take(MAX_USER_MESSAGES)
                    .map(|content| truncate(content, MAX_USER_MESSAGE_BYTES))
                    .collect::<Vec<_>>();
                json!({
                    "conversation_id": record.conversation_id,
                    "agent": record.agent,
                    "title": record.title,
                    "cwd": record.cwd,
                    "last_message_at": record.last_message_at.with_timezone(&timezone).format("%Y-%m-%d %H:%M").to_string(),
                    "user_messages": user_messages,
                    "user_messages_omitted": user_message_count.saturating_sub(MAX_USER_MESSAGES),
                    "last_assistant_reply": record
                        .last_assistant_reply
                        .map(|content| truncate(&content, MAX_LAST_REPLY_BYTES)),
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "week_start": week_start.with_timezone(&timezone).format("%Y-%m-%d %H:%M").to_string(),
            "timezone": timezone.name(),
            "conversation_count": conversations.len(),
            "conversations": conversations,
        }))
    }
}

fn truncate(content: &str, max_bytes: usize) -> String {
    let mut text = content.trim().to_string();
    if text.len() > max_bytes {
        utils::truncate_to_char_boundary(&mut text, max_bytes);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_appends_ellipsis_on_overflow() {
        let short = truncate("短内容", MAX_USER_MESSAGE_BYTES);
        assert_eq!(short, "短内容");

        let long = truncate(&"长".repeat(MAX_USER_MESSAGE_BYTES), MAX_USER_MESSAGE_BYTES);
        assert!(long.ends_with('…'));
        assert!(long.len() <= MAX_USER_MESSAGE_BYTES + '…'.len_utf8());
    }
}
