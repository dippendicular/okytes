#[test]
fn generated_v1_contract_matches_reviewed_baseline() {
    let current = serde_json::to_value(okytes_daemon::server::openapi()).unwrap();
    if std::env::var_os("OKYTES_UPDATE_API").is_some() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/api/openapi-v1.json"),
            serde_json::to_vec_pretty(&current).unwrap(),
        )
        .unwrap();
        return;
    }
    let baseline: serde_json::Value =
        serde_json::from_str(include_str!("../api/openapi-v1.json")).unwrap();
    assert_eq!(current,baseline,"API drift requires explicit review and regeneration; breaking changes require a new prefix/version");
    for (path, item) in current["paths"].as_object().unwrap() {
        assert!(path.starts_with("/v1/"));
        for (_, operation) in item.as_object().unwrap() {
            assert_eq!(
                operation["security"],
                serde_json::json!([{"bearer":[]}]),
                "{path}"
            );
        }
    }
    assert!(
        current["components"]["schemas"]["JobRecord"]["properties"]
            .get("headers")
            .is_none(),
        "credentials must not become public record fields"
    );
}
