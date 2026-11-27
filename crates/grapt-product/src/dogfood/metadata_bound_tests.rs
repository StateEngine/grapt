use super::*;

fn previews(bytes: usize) -> Value {
    json!([
        {"relativePath": "a.rs", "diff": {"oldText": "x".repeat(bytes), "newText": "y".repeat(bytes)}},
        {"relativePath": "b.rs", "diff": {"oldText": "x".repeat(bytes), "newText": "y".repeat(bytes)}},
    ])
}

#[test]
fn a_receipt_without_oversized_files_keeps_every_field() -> Result<(), String> {
    let metadata = json!({
        "durableCapture": "complete",
        "files": [{"relativePath": "a.rs", "patchOmitted": true}],
    });
    assert_eq!(bounded(metadata.clone()), metadata);
    Ok(())
}

#[test]
fn oversized_files_are_replaced_and_marked_with_what_went() -> Result<(), String> {
    let metadata = json!({"durableCapture": "complete", "files": previews(5_000)});
    assert!(
        !fits(&metadata),
        "the fixture must be over the reader's bound"
    );
    let bounded = bounded(metadata);
    assert!(fits(&bounded), "the bounded metadata must be readable");
    assert!(bounded.get("files").is_none());
    assert_eq!(bounded[DROPPED]["omitted"][0], "files");
    assert_eq!(bounded["durableCapture"], "complete");
    Ok(())
}

#[test]
fn oversized_nested_calls_are_replaced_and_marked() -> Result<(), String> {
    let calls: Vec<Value> = (0..40)
        .map(|index| json!({"name": "read", "detail": "d".repeat(300), "index": index}))
        .collect();
    let metadata = json!({"durableCapture": "complete", "toolCalls": calls});
    assert!(
        !fits(&metadata),
        "the fixture must be over the reader's bound"
    );
    let bounded = bounded(metadata);
    assert!(fits(&bounded), "the bounded metadata must be readable");
    assert!(bounded.get("toolCalls").is_none());
    assert_eq!(bounded[DROPPED]["omitted"][0], "toolCalls");
    Ok(())
}

/// When nothing optional explains the size, the marker alone keeps the row
/// readable, and it is still the one marker key.
#[test]
fn metadata_over_budget_on_its_own_keeps_only_the_marker() -> Result<(), String> {
    let metadata = json!({"durableCapture": "c".repeat(9_000)});
    assert!(
        !fits(&metadata),
        "the fixture must be over the reader's bound"
    );
    let bounded = bounded(metadata);
    assert!(fits(&bounded), "the marker alone must be readable");
    assert_eq!(bounded.as_object().map(|object| object.len()), Some(1));
    assert!(bounded.get(DROPPED).is_some());
    Ok(())
}

/// The fallback keeps both facts: the marker still names what was cut, even
/// when the remaining metadata alone is over the reader's bound.
#[test]
fn the_marker_alone_still_names_what_was_cut() -> Result<(), String> {
    let metadata = json!({
        "durableCapture": "complete",
        "files": previews(5_000),
        "unexplained": "c".repeat(9_000),
    });
    assert!(
        !fits(&metadata),
        "the fixture must be over the reader's bound"
    );
    let bounded = bounded(metadata);
    assert!(fits(&bounded), "the marker alone must be readable");
    assert_eq!(bounded.as_object().map(|object| object.len()), Some(1));
    assert_eq!(bounded[DROPPED]["omitted"][0], "files");
    Ok(())
}
