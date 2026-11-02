use glow_agent::{Tool, ToolCallError, ToolContext, tool};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[tool(name = "lookup", description = "Look up recent values")]
async fn lookup(days: Option<u32>, market: String) -> Result<Value, ToolCallError> {
    Ok(serde_json::to_value(json!({
        "days": days,
        "market": market
    }))?)
}

#[tokio::test]
async fn generates_metadata_schema_and_argument_conversion() {
    let spec = lookup.spec();
    assert_eq!(spec.name, "lookup");
    assert_eq!(spec.description, "Look up recent values");
    assert!(spec.input_schema["properties"]["days"].is_object());
    assert!(spec.input_schema["properties"]["market"].is_object());
    assert_eq!(spec.input_schema["required"], json!(["market"]));

    let result = lookup
        .call(
            ToolContext {
                run_id: Uuid::new_v4(),
                cancellation_token: CancellationToken::new(),
            },
            json!({ "market": "tokyo" }),
        )
        .await
        .unwrap();

    assert_eq!(result, json!({ "days": null, "market": "tokyo" }));
}

#[tokio::test]
async fn reports_missing_and_invalid_arguments() {
    let context = || ToolContext {
        run_id: Uuid::new_v4(),
        cancellation_token: CancellationToken::new(),
    };

    let missing = lookup.call(context(), json!({})).await.unwrap_err();
    assert!(
        missing
            .message
            .contains("missing required argument `market`")
    );

    let invalid = lookup
        .call(context(), json!({ "days": "many", "market": "tokyo" }))
        .await
        .unwrap_err();
    assert!(invalid.message.contains("invalid argument `days`"));
}

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct Segment {
    from: Point,
    to: Point,
}

/// Measure a segment.
#[tool]
async fn measure(segment: Segment, context: ToolContext) -> Result<Value, ToolCallError> {
    Ok(json!({
        "dx": segment.to.x - segment.from.x,
        "dy": segment.to.y - segment.from.y,
        "cancelled": context.cancellation_token.is_cancelled()
    }))
}

#[tokio::test]
async fn nested_types_share_root_definitions_and_context_is_injected() {
    let spec = measure.spec();
    assert_eq!(spec.description, "Measure a segment.");
    let schema = &spec.input_schema;
    // The context parameter is not part of the model-facing schema.
    assert!(schema["properties"].get("context").is_none());
    assert_eq!(schema["required"], json!(["segment"]));
    assert_eq!(schema["properties"]["segment"]["$ref"], "#/$defs/Segment");
    assert_eq!(
        schema["$defs"]["Segment"]["properties"]["from"]["$ref"],
        "#/$defs/Point"
    );
    assert!(schema["$defs"]["Point"].is_object());
    let validator = jsonschema::validator_for(schema).unwrap();
    let valid = json!({"segment": {"from": {"x": 0, "y": 0}, "to": {"x": 3, "y": 4}}});
    assert!(validator.is_valid(&valid));
    assert!(!validator.is_valid(&json!({"segment": {"from": {"x": 0}, "to": {}}})));

    let token = CancellationToken::new();
    token.cancel();
    let result = measure
        .call(
            ToolContext {
                run_id: Uuid::new_v4(),
                cancellation_token: token,
            },
            valid,
        )
        .await
        .unwrap();
    assert_eq!(result, json!({"dx": 3, "dy": 4, "cancelled": true}));
}
