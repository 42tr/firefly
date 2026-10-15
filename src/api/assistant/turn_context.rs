use super::*;

/// Restore a legacy completed reply's summary, in execution order, and leave
/// later records (including interrupted tool calls) for the normal loader.
pub(super) fn restore(history: &mut Vec<assistant_store::MessageRecord>) -> Option<Message> {
    let (index, content) = history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, record)| {
            if record.role != "assistant"
                || record.is_error
                || record.content.trim().is_empty()
                || record
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls != &json!([]))
                || record
                    .processing_status
                    .as_deref()
                    .is_some_and(|status| status != "completed")
            {
                return None;
            }
            let summary = record.metadata.as_ref()?.get("context_summary")?;
            if summary["version"].as_u64() != Some(1) {
                return None;
            }
            let content = summary["content"].as_str()?.trim();
            (!content.is_empty()).then(|| (index, content.to_owned()))
        })?;
    history.drain(..=index);
    Some(Message::assistant(content, None))
}

#[cfg(test)]
mod tests;
