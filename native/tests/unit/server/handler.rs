use super::*;

#[test]
fn error_record_preserves_sanitized_upstream_details() {
    let shape = json!({"modelFamily": "glm-5"});
    let error = AppError::new(StatusCode::NOT_ACCEPTABLE, "Not Acceptable").with_details(json!({
        "response": "Not Acceptable",
        "upstreamRequestShape": shape.clone(),
    }));

    let record = error_record_value("req_1", "https://provider.test/v1", "glm-5.2", true, &error);

    assert_eq!(record["error"]["status"], 406);
    assert_eq!(record["error"]["response"], "Not Acceptable");
    assert_eq!(record["upstreamRequestShape"], shape);
}
