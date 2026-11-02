use std::collections::BTreeMap;

use serde_json::{json, Value};

/// A result closes a model turn, not necessarily the work requested by the user.
/// Session idle (including background agents), the final result, and an empty
/// live task set must agree before the host may release a generation job.
#[derive(Default)]
pub struct Activity {
    tasks: BTreeMap<String, Value>,
    result: bool,
    idle: bool,
    seen_state: bool,
    processing_results: bool,
    requires_action: bool,
}

impl Activity {
    pub fn begin_prompt(&mut self) {
        self.result = false;
        self.idle = false;
        self.processing_results = false;
        self.requires_action = false;
    }

    pub fn observe(&mut self, event: &Value) {
        let root = event["parent_tool_use_id"].is_null() && event["agent_id"].is_null();
        match event["type"].as_str() {
            Some("result") if root => {
                self.result = true;
                self.processing_results = false;
            }
            Some("stream_event") if root && event["event"]["type"] == "message_start" => {
                self.result = false;
                self.idle = false;
            }
            Some("system") => match event["subtype"].as_str() {
                Some("session_state_changed") => {
                    self.seen_state = true;
                    self.idle = event["state"] == "idle";
                    self.requires_action = event["state"] == "requires_action";
                }
                Some("background_tasks_changed") => {
                    let had_tasks = !self.tasks.is_empty();
                    // This is the whole live set, not a delta. Ignore ambient
                    // provider tasks that aren't work requested by the user.
                    self.tasks = event["tasks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|task| task["ambient"] != true)
                        .filter_map(|task| {
                            Some((task["task_id"].as_str()?.to_owned(), task.clone()))
                        })
                        .collect();
                    // Older CLIs can publish the empty set before the task's
                    // notification. Never let a previous idle/result pair win
                    // that gap before the main agent consumes the report.
                    if had_tasks && self.tasks.is_empty() {
                        self.result = false;
                        self.idle = false;
                        self.processing_results = true;
                    }
                }
                Some("task_started")
                    if event["ambient"] != true && event["is_backgrounded"] == true =>
                {
                    if let Some(id) = event["task_id"].as_str() {
                        self.tasks.insert(id.into(), event.clone());
                    }
                }
                Some("task_notification") if event["ambient"] != true => {
                    if let Some(id) = event["task_id"].as_str() {
                        // A stale completion from an earlier run must not hide a resumed task.
                        let stale = self.tasks.get(id).is_some_and(|task| {
                            task["run_id"]
                                .as_str()
                                .zip(event["run_id"].as_str())
                                .is_some_and(|(current, received)| current > received)
                        });
                        if stale {
                            return;
                        }
                        self.tasks.remove(id);
                    }
                    self.result = false;
                    self.idle = false;
                    self.processing_results = true;
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn complete(&self) -> bool {
        self.result && self.idle && self.tasks.is_empty()
    }

    pub fn missing_session_state(&self) -> bool {
        self.result && !self.seen_state
    }

    pub fn snapshot(&self) -> Value {
        let tasks: Vec<_> = self
            .tasks
            .iter()
            .map(|(id, task)| {
                json!({
                    "id":id, "description":task["description"], "task_type":task["task_type"],
                })
            })
            .collect();
        json!({
            "phase": if self.requires_action { "requires_action" }
                else if self.processing_results { "processing_results" }
                else if self.result && !tasks.is_empty() { "waiting_background" }
                else { "running" },
            "background_count":tasks.len(), "tasks":tasks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(activity: &mut Activity, state: &str) {
        activity.observe(&json!({"type":"system","subtype":"session_state_changed","state":state}));
    }
    fn result(activity: &mut Activity) {
        activity.observe(&json!({"type":"result","subtype":"success"}));
    }

    #[test]
    fn background_completion_must_be_processed_before_finishing() {
        let mut activity = Activity::default();
        state(&mut activity, "running");
        activity.observe(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"child","task_type":"local_agent"}]}));
        result(&mut activity);
        assert_eq!(activity.snapshot()["phase"], "waiting_background");
        assert!(!activity.complete());
        // Even an out-of-order idle event cannot complete a live task.
        state(&mut activity, "idle");
        assert!(!activity.complete());
        activity.observe(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}));
        assert!(!activity.complete());
        activity.observe(&json!({"type":"system","subtype":"task_notification","task_id":"child","status":"completed"}));
        activity.observe(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}));
        assert_eq!(activity.snapshot()["phase"], "processing_results");
        assert!(!activity.complete());
        // Child output/results must not finish the parent's request.
        activity.observe(&json!({"type":"result","parent_tool_use_id":"delegate"}));
        assert!(!activity.complete());
        result(&mut activity);
        assert!(!activity.complete());
        state(&mut activity, "idle");
        assert!(activity.complete());
    }

    #[test]
    fn idle_and_result_can_arrive_in_either_order_and_snapshots_correct_missing_events() {
        for idle_first in [false, true] {
            let mut activity = Activity::default();
            activity.observe(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"watcher","ambient":true}]}));
            if idle_first {
                state(&mut activity, "idle");
                result(&mut activity);
            } else {
                result(&mut activity);
                state(&mut activity, "idle");
            }
            assert!(activity.complete());
        }
        let mut activity = Activity::default();
        activity.observe(&json!({"type":"system","subtype":"task_started","task_id":"child","run_id":"b","is_backgrounded":true}));
        activity.observe(
            &json!({"type":"system","subtype":"task_notification","task_id":"child","run_id":"a"}),
        );
        assert_eq!(activity.snapshot()["background_count"], 1);
        activity.observe(&json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}));
        assert_eq!(activity.snapshot()["background_count"], 0);
    }

    #[test]
    fn running_before_input_acknowledgment_confirms_state_support_without_finishing_new_prompt() {
        let mut activity = Activity::default();
        state(&mut activity, "idle");
        activity.begin_prompt();
        result(&mut activity);
        assert!(!activity.complete());
        assert!(!activity.missing_session_state());
        state(&mut activity, "idle");
        assert!(activity.complete());
    }
}
