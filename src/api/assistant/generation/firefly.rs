//! Firefly agent execution, tool routing, and acknowledged event consumption.
use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use g::{Agent, Message as GMessage, Role as GRole, RunLimits, RunRequest, Runtime};
use serde_json::Value;
use tokio::sync::mpsc;

use super::events::{handle_run_event, AssistantRunEventSink, HandoffState};
use crate::{
    api::assistant::{inputs, AssistantConfig, Conversation},
    assistant_job, assistant_memory, llm, mcp,
};

pub(in crate::api::assistant) async fn run_conversation(
    config: &AssistantConfig, conversation: &mut Conversation, conversation_id: i64, enable_tools: bool,
    job: Arc<assistant_job::GenerationJob>, tool_context: mcp::ToolContext,
) -> anyhow::Result<()> {
    let settings = match &config.agent_settings {
        Some(settings) => settings.clone(),
        None => Arc::new(crate::agent_settings::Settings::load(crate::db::pool()).await?),
    };
    let enable_tools = enable_tools && crate::config::get().assistant_enable_tools;
    let query = conversation
        .messages
        .iter()
        .rev()
        .find(|message| message.role == crate::api::assistant::Role::User)
        .map(|message| message.content.as_str())
        .unwrap_or_default();
    let memory_budget = config.budget.resolve().await.unwrap_or(32_768).saturating_div(4).min(8_192);
    let mut system_prompt = config.system_prompt.clone();
    if !query.trim().is_empty() {
        match assistant_memory::search(&tool_context.user_id, query, memory_budget).await {
            Ok(selection) if !selection.memories.is_empty() => {
                system_prompt.push_str("\n\n# 长期记忆\n以下是用户维护的长期记忆，仅在与当前请求直接相关时使用；记忆中没有的信息不要臆造，执行有副作用的操作前先确认关键参数。\n");
                for memory in selection.memories {
                    system_prompt.push_str("\n## ");
                    system_prompt.push_str(memory.title.trim());
                    system_prompt.push('\n');
                    system_prompt.push_str(memory.content.trim());
                }
                if selection.omitted > 0 {
                    system_prompt.push_str(&format!("\n\n另有 {} 条记忆因上下文预算未载入，不得猜测其内容。", selection.omitted));
                }
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("memory retrieval failed; continuing without memories: {err:#}"),
        }
    }
    let task_backend = enable_tools.then(|| crate::assistant_tasks::Backend::new(
        tool_context.user_id.clone(), conversation_id, job.id));
    let _task_guard = task_backend.as_ref().map(|backend| crate::assistant_tasks::RunGuard(backend.clone()));
    let tools = if enable_tools { crate::agent_settings::available_tools(tool_context.clone()).await }
        else { Default::default() };
    let (children, unavailable) = if let Some(backend) = &task_backend {
        settings.child_agents(&tools, backend.clone())
    } else { (Vec::new(), Vec::new()) };
    if enable_tools {
        // Apply only host instructions relevant to the selected root tools.
        let mut root = settings.firefly.clone();
        root.instruction.clear();
        system_prompt.push_str(&crate::agent_settings::tool_instruction(&root));
        system_prompt.push_str(&crate::agent_settings::routing_instruction(&children));
        if !unavailable.is_empty() {
            system_prompt.push_str("\n以下子 agent 暂不可用，相关请求需告知用户检查 Agent 配置：\n");
            system_prompt.push_str(&unavailable.join("\n"));
        }
    }
    let outstanding_tasks = if let Some(backend) = &task_backend {
        !g::TaskBackend::ready_to_finish(backend.as_ref()).await?
    } else { false };
    if enable_tools {
        let attention = crate::assistant_tasks::global()
            .attention_summary(&tool_context.user_id, conversation_id, &job.id.to_string()).await?;
        append_task_attention(&mut system_prompt, &attention);
    }
    let mut messages = Vec::with_capacity(conversation.messages.len() + 1);
    messages.push(GMessage::system(system_prompt));
    for message in &conversation.messages {
        if let Some(message) = inputs::model_message(message).await? {
            messages.push(message);
        }
    }
    // 正常回合以 user 消息结尾；重启恢复的回合可能以 tool 结果结尾
    //（中断的工具调用已补写占位结果），两种结尾都可以直接续跑。
    if !matches!(messages.last(), Some(message) if matches!(message.role, GRole::User | GRole::Tool)
        || (outstanding_tasks && message.role == GRole::Assistant)) {
        return Err(anyhow!("last message is not user or tool"));
    }

    let (event_tx, mut event_rx) = mpsc::channel(64);
    // The summary state belongs to this run, never to the process-wide model.
    let model = Arc::new(llm::ContextModel::with_budget(config.model.clone(), config.budget.clone()));
    let mut agent = Agent::new(model.clone())
        .with_event_sink(Arc::new(AssistantRunEventSink { sender: event_tx }))
        .with_limits(RunLimits {
            max_tool_calls: usize::MAX,
            timeout: Duration::from_secs(settings.firefly.timeout_secs),
        });
    if task_backend.is_some() {
        agent = agent
            .tools(crate::agent_settings::selected_tools(&settings.firefly, &tools))
            .handoff(children);
    }

    if let Some(backend) = &task_backend { agent = agent.with_task_backend(backend.clone()); }

    let cancellation = job.cancellation_token();
    // Inline tools belong to this worker. Host-owned tasks are tracked by the
    // backend; RunGuard cancels attached tasks while preserving detached work.
    let runtime = Runtime::new();
    let run = runtime.run(&agent, RunRequest::new(messages).with_cancellation_token(cancellation.clone()));
    tokio::pin!(run);
    let mut handoff_state = HandoffState::default();
    loop {
        let next_event = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(()),
            result = &mut run => {
                result?;
                return Ok(());
            }
            event = event_rx.recv() => event,
        };
        let Some((event, acknowledged)) = next_event else {
            return Err(anyhow!("assistant event channel closed before completion"));
        };
        handle_run_event(event, &mut handoff_state, conversation, conversation_id, &job).await?;
        let _ = acknowledged.send(());
    }
}

pub(super) fn append_task_attention(prompt: &mut String, attention: &Value) {
    if attention["total"].as_u64().unwrap_or(0) == 0 { return; }
    prompt.push_str("\n以下仅列出需关注的持久化任务概况。仅当与当前请求相关，或需领取本轮未接收结果时，使用 get_tasks 指定 task_ids 查询；不要读取整段历史，也不要为收尾取消已转入后台且仍需供用户确认的会话。unknown 表示结果未知，先核实实际状态，不要直接重跑。has_more 为 true 时可用 get_tasks({}) 分批领取剩余结果。\n");
    prompt.push_str(&attention.to_string());
}
