//! A same-length flip of carried bytes: reads check structure only and never
//! serve the flipped bytes; writer admission still re-digests every object.
use super::{
    endpoint, establish_carry, launch_serve, request, reset, root, wait_drained, wait_ready, Carry,
};
use std::fs;
use std::path::Path;

/// Rewrite one letter of every carried object in place, keeping its length
/// and its record, and return a window of each object's corrupted bytes.
fn flip_carried_objects(artifacts: &Path, references: &[grapt_types::ArtifactRef]) -> Vec<String> {
    let ids: std::collections::BTreeSet<&str> = references
        .iter()
        .map(|reference| reference.id().as_str())
        .collect();
    let mut pending = vec![artifacts.join("objects")];
    let mut windows = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("artifact objects unreadable: {error}"));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("artifact entry unreadable: {error}"))
                .path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| ids.contains(name))
            {
                continue;
            }
            let mut bytes =
                fs::read(&path).unwrap_or_else(|error| panic!("object unreadable: {error}"));
            let at = bytes
                .iter()
                .position(u8::is_ascii_lowercase)
                .unwrap_or_else(|| panic!("carried object has no letter to flip"));
            if let Some(byte) = bytes.get_mut(at) {
                *byte = if *byte == b'z' { b'a' } else { *byte + 1 };
            }
            fs::write(&path, &bytes).unwrap_or_else(|error| panic!("object rewrite: {error}"));
            let window = bytes.get(at.saturating_sub(8)..bytes.len().min(at + 16));
            windows.push(String::from_utf8_lossy(window.unwrap_or_default()).into_owned());
        }
    }
    assert_eq!(windows.len(), ids.len(), "every carried object was flipped");
    windows
}

#[test]
fn a_same_length_flip_is_digested_where_it_is_served_and_at_writer_admission() {
    let root = root("j02-carry-flip");
    reset(&root);
    let Carry {
        session_id,
        references,
        _provider,
    } = establish_carry(&root);
    // Same length, intact record: only a byte digest can see this corruption.
    let windows = flip_carried_objects(&root.join("dogfood-artifacts"), &references);
    let mut product = launch_serve(&root);
    let ready = wait_ready(&root, &mut product);
    let address = endpoint(&ready);
    // A read checks records, objects and lengths, never the Session's bytes:
    // the Session opens. Whatever a read serves passes through the verified
    // fetch, so a flipped object is never returned as content.
    let (status, body) = request(address, "GET", &format!("/api/session/{session_id}"), None);
    assert!(
        (200..300).contains(&status),
        "structural read refused: {body:?}"
    );
    let (status, body) = request(
        address,
        "GET",
        &format!("/api/session/{session_id}/message"),
        None,
    );
    let text = body.map(|body| body.to_string()).unwrap_or_default();
    // The page renders the carried content through the fetch, whose digest
    // refuses the flipped object: the page is refused by the row's name.
    assert!(
        status >= 400 && text.contains("history row") && text.contains("artifact is unavailable"),
        "history did not refuse the flipped content by name: {status} {text}"
    );
    for window in &windows {
        assert!(
            !text.contains(window.as_str()),
            "flipped bytes served: {window:?} in {text}"
        );
    }
    // Writer admission still re-digests every carried object.
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session_id}/prompt"),
        Some(r#"{"text":"must refuse"}"#),
    );
    let text = body.map(|body| body.to_string()).unwrap_or_default();
    assert!(status >= 400, "corrupt carry resumed: {text}");
    assert!(
        text.contains("carry refused") && text.contains("corrupt"),
        "{text}"
    );
    assert!(wait_drained(&mut product).status.success());
    reset(&root);
}
