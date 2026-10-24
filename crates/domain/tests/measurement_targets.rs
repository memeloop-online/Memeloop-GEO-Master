use geo_domain::{ErrorCode, ProjectSettings};
use serde_json::json;

#[test]
fn measurement_targets_are_explicit_trimmed_and_serialize_in_current_settings() {
    let empty = ProjectSettings {
        brand_name: "Enterprise".to_owned(),
        product_name: Some("Enterprise model".to_owned()),
        ..ProjectSettings::default()
    };
    assert_eq!(
        serde_json::to_value(&empty).unwrap()["measurement_targets"],
        json!([])
    );
    let settings: ProjectSettings = serde_json::from_value(json!({
        "measurement_targets": [
            {"name":" Model A ","kind":"product","aliases":[" A "]},
            {"name":"Model A Pro","kind":"product","aliases":[]},
            {"name":"Model A","kind":"brand","aliases":["A"]},
            {"name":"Kitchen","kind":"restaurant","aliases":[]},
            {"name":"Care","kind":"service","aliases":[]},
            {"name":"Venue","kind":"other","aliases":[]}
        ]
    }))
    .unwrap();
    let normalized = settings.validate_draft().unwrap();
    assert_eq!(normalized.measurement_targets[0].name, "Model A");
    assert_eq!(normalized.measurement_targets[0].aliases, ["A"]);
    assert_eq!(normalized.measurement_targets.len(), 6);
    assert!(normalized.brand_name.is_empty());
    assert!(normalized.product_name.is_none());
    let encoded = serde_json::to_value(&normalized).unwrap();
    assert_eq!(
        serde_json::from_value::<ProjectSettings>(encoded).unwrap(),
        normalized
    );
}

#[test]
fn target_validation_rejects_ambiguous_exact_terms_within_a_kind() {
    for second in [
        json!({"name":"A","kind":"product","aliases":[]}),
        json!({"name":"Model B","kind":"product","aliases":[" Model A "]}),
        json!({"name":"Model B","kind":"product","aliases":["A"]}),
    ] {
        let settings: ProjectSettings = serde_json::from_value(json!({
            "measurement_targets": [
                {"name":"Model A","kind":"product","aliases":["A"]}, second
            ]
        }))
        .unwrap();
        let error = settings.validate_draft().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.details.unwrap()["field"], "measurement_targets");
    }
}

#[test]
fn target_validation_rejects_blank_and_oversized_terms_and_unknown_shapes() {
    for target in [
        json!({"name":" ","kind":"product","aliases":[]}),
        json!({"name":"Model","kind":"product","aliases":[" "]}),
        json!({"name":"x".repeat(201),"kind":"product","aliases":[]}),
        json!({"name":"Model","kind":"product","aliases":["x".repeat(201)]}),
    ] {
        let settings: ProjectSettings = serde_json::from_value(json!({
            "measurement_targets": [target]
        }))
        .unwrap();
        assert_eq!(
            settings.validate_draft().unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
    for target in [
        json!({"name":"Model","kind":"unknown","aliases":[]}),
        json!({"name":"Model","kind":"product"}),
        json!({"name":"Model","kind":"product","aliases":[],"brand":"Example"}),
    ] {
        assert!(
            serde_json::from_value::<ProjectSettings>(json!({
                "measurement_targets": [target]
            }))
            .is_err()
        );
    }
}
