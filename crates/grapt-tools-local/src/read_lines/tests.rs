//! Zero coordinates are refused by the reader itself, not only by its caller.

use super::*;

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("grapt-read-lines-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("scratch: {error}"));
    let path = root.join("file.txt");
    std::fs::write(&path, b"first\nsecond\n").unwrap_or_else(|error| panic!("write: {error}"));
    path
}

#[test]
fn zero_start_coordinates_are_refused_rather_than_underflowing() {
    let path = scratch("zero");
    let root = path
        .parent()
        .unwrap_or_else(|| panic!("fixture has no parent"));

    for (line, column) in [(0, 1), (1, 0), (0, 0)] {
        match page(root, &path, line, column, 1) {
            Ok(_) => panic!("zero start coordinates {line}:{column} built a page"),
            Err(error) => assert!(
                error.contains("1-based start coordinates"),
                "unexpected refusal for {line}:{column}: {error}"
            ),
        }
    }

    let _ = std::fs::remove_dir_all(root);
}
