//! Tool arguments are executable input. Normalize wrappers only; never guess values
//! or repair strings, escapes, missing delimiters, or an incomplete JSON document.
use serde_json::{json, Value};

const MAX_EXTRA_CLOSERS: usize = 8;
const MAX_SCHEMA_ERRORS: usize = 8;

pub(super) struct ParsedArguments {
    pub value: Value,
    pub normalizations: Vec<&'static str>,
}

pub(super) struct ArgumentError {
    pub message: String,
    pub feedback: Value,
}

pub(super) fn parse(raw: &str) -> Result<ParsedArguments, ArgumentError> {
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
pub(super) fn schema_error(validator: &jsonschema::Validator, arguments: &Value) -> Option<ArgumentError> {
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
}
