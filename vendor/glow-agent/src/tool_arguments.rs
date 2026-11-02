//! Tool arguments are executable input. Normalize wrappers only; never guess values
//! or repair strings, escapes, missing delimiters, or an incomplete JSON document.
//! A tool name that a run_tools call omitted is the one exception: it is filled in only
//! when the call's arguments validate against exactly one candidate schema.
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::ToolSpec;

const MAX_EXTRA_CLOSERS: usize = 8;
const MAX_SCHEMA_ERRORS: usize = 8;

pub(crate) struct ParsedArguments {
    pub value: Value,
    pub normalizations: Vec<&'static str>,
}

pub(crate) struct ArgumentError {
    pub message: String,
    pub feedback: Value,
}

pub(crate) fn parse(raw: &str) -> Result<ParsedArguments, ArgumentError> {
    let mut normalizations = Vec::new();
    let trimmed = raw.trim();
    let candidate = trimmed.strip_prefix("```json\n")
        .or_else(|| trimmed.strip_prefix("```\n"))
        .and_then(|text| text.strip_suffix("\n```"));
    let candidate = match candidate {
        Some(text) => {
            normalizations.push("json_code_fence");
            text.trim()
        }
        None => trimmed,
    };
    let value = match serde_json::from_str::<Value>(candidate) {
        Ok(value) => value,
        Err(error) => {
            // A streaming deserializer finds the end of a COMPLETE first value,
            // correctly handling braces, Unicode and escapes inside strings.
            let mut values = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
            let first = values.next().and_then(Result::ok);
            let suffix = &candidate[values.byte_offset()..];
            let closers = suffix.bytes().filter(|byte| !byte.is_ascii_whitespace()).count();
            if first.as_ref().is_some_and(Value::is_object)
                && (1..=MAX_EXTRA_CLOSERS).contains(&closers)
                && suffix.bytes().all(|byte| byte == b'}' || byte.is_ascii_whitespace()) {
                normalizations.push("extra_object_closers");
                first.unwrap()
            } else {
                let (kind, hint) = if error.is_eof() {
                    ("incomplete_json", "The JSON document or string is incomplete. Regenerate the full arguments object; do not guess or append missing command text.")
                } else if first.is_some() {
                    ("trailing_data", "Return exactly one JSON object. Remove trailing text or additional JSON values; do not concatenate separate calls after the object.")
                } else {
                    ("invalid_json", "Regenerate a valid JSON object. Check commas, quoted property names and balanced delimiters. Encode command strings with JSON escaping exactly once; preserve the intended command.")
                };
                return Err(ArgumentError {
                    message: error.to_string(),
                    feedback: json!({"stage":"json", "kind":kind, "line":error.line(),
                        "column":error.column(), "hint":hint}),
                });
            }
        }
    };
    if !value.is_object() {
        return Err(ArgumentError {
            message: "工具参数必须是 JSON 对象".into(),
            feedback: json!({"stage":"schema", "kind":"object_required", "path":"",
                "hint":"Return an arguments object, not a string, array, number or null."}),
        });
    }
    Ok(ParsedArguments { value, normalizations })
}

/// Include field locations and fixes, but never echo actual argument values into
/// logs or user-facing errors. The rejected batch itself is sent only to the model.
pub(crate) fn schema_error(validator: &jsonschema::Validator, arguments: &Value) -> Option<ArgumentError> {
    use jsonschema::error::ValidationErrorKind;
    let errors: Vec<_> = validator.iter_errors(arguments).take(MAX_SCHEMA_ERRORS).map(|error| {
        let hint = match &error.kind {
            ValidationErrorKind::Required { property } => format!(
                "Add the required property {property} at this path using the tool schema and original task. Do not invent command text or tool names."),
            ValidationErrorKind::Type { .. } => "Use the JSON type declared in the schema at this path; do not encode objects or arrays as strings.".into(),
            ValidationErrorKind::AdditionalProperties { .. } => "Use only properties declared in the schema at this path.".into(),
            _ => "Correct this field to conform to the declared schema while preserving the original task.".into(),
        };
        json!({"path":error.instance_path.to_string(), "schema_path":error.schema_path.to_string(),
            "error":error.masked().to_string(), "hint":hint})
    }).collect();
    let first = errors.first()?;
    Some(ArgumentError {
        message: format!("参数路径 {}：{}", first["path"].as_str().unwrap_or_default(),
            first["error"].as_str().unwrap_or_default()),
        feedback: json!({"stage":"schema", "kind":"schema_validation", "errors":errors}),
    })
}

/// Fill in a `tool` name the model omitted, but only when the call's `arguments`
/// validate against exactly one candidate schema. The name comes from the same
/// schemas used for validation, never from key names or string heuristics;
/// ambiguous or unmatched batches stay invalid and go back through the repair loop.
pub(crate) fn infer_missing_tool_names(value: &mut Value, candidates: &[ToolSpec]) -> Vec<String> {
    let Some(calls) = value.get_mut("calls").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    // Task controls cannot be nested inside a batch, so they are never candidates.
    let usable: Vec<&ToolSpec> = candidates
        .iter()
        .filter(|spec| !crate::task::is_task_control(&spec.name))
        .collect();
    let mut validators: HashMap<&str, Option<jsonschema::Validator>> = HashMap::new();
    let mut inferred = Vec::new();
    for (index, call) in calls.iter_mut().enumerate() {
        let Some(call) = call.as_object_mut() else { continue };
        if call.get("tool").and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty()) {
            continue;
        }
        let Some(arguments) = call.get("arguments").filter(|value| value.is_object()).cloned() else {
            continue;
        };
        let mut matches = Vec::new();
        for spec in &usable {
            let accepts = validators
                .entry(spec.name.as_str())
                .or_insert_with(|| jsonschema::validator_for(&spec.input_schema).ok())
                .as_ref()
                .is_some_and(|validator| validator.validate(&arguments).is_ok());
            if accepts {
                matches.push(spec.name.as_str());
            }
        }
        if let [name] = matches[..] {
            tracing::info!(tool_name = name, call_index = index, "inferred missing task tool from schema");
            call.insert("tool".into(), Value::String(name.to_string()));
            inferred.push(format!("inferred_missing_tool:{name}"));
        }
    }
    inferred
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_objects_preserve_command_strings_when_normalizing_wrappers() {
        let command = "echo \"中文 } ] \\\n\"; printf '%s' '{\"nested\":true}'; echo '$HOME'";
        let expected = json!({"calls":[{"tool":"command","title":"检查命令","arguments":{"command":command}}],
            "yield_when":"any"});
        let raw = expected.to_string();
        for (wrapped, count) in [
            (raw.clone(), 0), (format!(" \n{raw}\n"), 0),
            (format!("{raw}}}"), 1), (format!("{raw} }} \n }}"), 1),
            (format!("```json\n{raw}\n```"), 1),
            (format!("```\n{raw}}}\n```"), 2),
        ] {
            let parsed = parse(&wrapped).unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(parsed.value, expected);
            assert_eq!(parsed.normalizations.len(), count);
            assert_eq!(parsed.value["calls"][0]["arguments"]["command"], command);
        }
    }

    #[test]
    fn ambiguous_truncated_and_non_object_arguments_are_never_salvaged() {
        for raw in [
            "", "{", r#"{"command":"echo"#,
            r#"{"command":"echo}"#, r#"{"command":"bad\q"}"#,
            "{}{}", "{} null", "{} } {}", "{}]", "{} trailing text",
            "{} } trailing text", "{} }}}}}}}}}", "{\"calls\":[{}]", "{\"calls\":[{},]}",
            "[]}", "[]", "null", "1", "\"{}\"", "text ```json\n{}\n```",
            "```json\n{}\n``` trailing text", "```json\n{}\n```\n```json\n{}\n```",
        ] {
            assert!(parse(raw).is_err(), "unexpectedly accepted {raw}");
        }
        assert_eq!(parse(r#"{"command":"unfinished"#).err().unwrap().feedback["kind"], "incomplete_json");
        assert_eq!(parse("{}{}").err().unwrap().feedback["kind"], "trailing_data");
    }

    #[test]
    fn schema_feedback_identifies_missing_fields_and_masks_argument_values() {
        let validator = jsonschema::validator_for(&json!({"type":"object","properties":{
            "calls":{"type":"array","items":{"type":"object","properties":{
                "title":{"type":"string"},"tool":{"type":"string"}},
                "required":["title","tool"],"additionalProperties":false}}},"required":["calls"]})).unwrap();
        let error = schema_error(&validator, &json!({"calls":[{"tool":"command"},
            {"tool":null,"title":{"secret":"DO_NOT_LOG"}}]})).unwrap();
        let feedback = error.feedback.to_string();
        assert!(feedback.contains("/calls/0"));
        assert!(feedback.contains("title"));
        assert!(feedback.contains("Add the required property"));
        assert!(feedback.contains("/calls/1/title"));
        assert!(!feedback.contains("DO_NOT_LOG"));
        assert!(!error.message.contains("DO_NOT_LOG"));
        assert!(schema_error(&validator, &json!({"calls":[{"tool":"command","title":"运行检查"}]})).is_none());
    }

    fn fixture_tool(name: &str, input_schema: Value) -> ToolSpec {
        ToolSpec { name: name.into(), description: "fixture".into(), input_schema, behavior: Default::default() }
    }

    fn command_schema() -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"}},
            "required":["command"],"additionalProperties":false})
    }

    fn fixture_candidates() -> Vec<ToolSpec> {
        vec![
            fixture_tool("run_tools", json!({"type":"object"})),
            fixture_tool("command", command_schema()),
            fixture_tool("get_tasks", json!({"type":"object","properties":{"task_ids":{"type":"array","items":{"type":"string"}}},
                "required":["task_ids"],"additionalProperties":false})),
        ]
    }

    #[test]
    fn a_missing_tool_name_is_inferred_only_from_exactly_one_matching_schema() {
        let mut batch = json!({"calls":[{"title":"运行检查","arguments":{"command":"echo hi"}}]});
        assert_eq!(infer_missing_tool_names(&mut batch, &fixture_candidates()), vec!["inferred_missing_tool:command"]);
        assert_eq!(batch["calls"][0]["tool"], "command");
        let validator = jsonschema::validator_for(&json!({"type":"object","properties":{"calls":{"type":"array",
            "items":{"type":"object","properties":{"tool":{"type":"string"},"arguments":{"type":"object"}},
            "required":["tool","arguments"]}}},"required":["calls"]})).unwrap();
        assert!(schema_error(&validator, &batch).is_none());
    }

    #[test]
    fn ambiguous_unmatched_absent_or_present_calls_are_never_inferred() {
        let expected = json!({"calls":[{"title":"运行检查","arguments":{"command":"echo hi"}}]});
        let mut ambiguous = expected.clone();
        let mut many = fixture_candidates();
        many.push(fixture_tool("shell", command_schema()));
        assert!(infer_missing_tool_names(&mut ambiguous, &many).is_empty());
        assert_eq!(ambiguous, expected);
        let expected = json!({"calls":[{"title":"运行检查","arguments":{}},{"title":"运行检查"},
            {"title":"运行检查","arguments":"echo"},
            {"tool":"command","title":"运行检查","arguments":{"command":"echo hi"}}]});
        let mut batch = expected.clone();
        assert!(infer_missing_tool_names(&mut batch, &fixture_candidates()).is_empty());
        assert_eq!(batch, expected);
        let expected = json!({"calls":[{"title":"取结果","arguments":{"task_ids":["a"]}}]});
        let mut control = expected.clone();
        assert!(infer_missing_tool_names(&mut control, &fixture_candidates()).is_empty());
        assert_eq!(control, expected);
    }
}
