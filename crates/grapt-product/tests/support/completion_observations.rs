//! Shared assertion for durable assistant completion timestamps.

pub(super) fn assert_completion_observations(messages: &serde_json::Value) {
    let rows = messages["data"]
        .as_array()
        .unwrap_or_else(|| panic!("completion witness has no message array"));
    let assistants = rows
        .iter()
        .filter(|row| row["type"] == "assistant")
        .collect::<Vec<_>>();
    assert!(
        !assistants.is_empty(),
        "completion witness has no assistant"
    );
    for row in assistants {
        let created = row["time"]["created"]
            .as_f64()
            .unwrap_or_else(|| panic!("completion witness has no created instant"));
        let completed = row["time"]["completed"]
            .as_f64()
            .unwrap_or_else(|| panic!("finished assistant has no completion observation"));
        assert!(
            completed > 0.0 && completed >= created,
            "public completion reused a pre-opening reservation instead of settlement"
        );
    }
}
