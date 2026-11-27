use super::*;

#[test]
fn status_fields_are_literal_and_every_row_fits_the_viewport() {
    let snapshot = Snapshot {
        fields: vec![("Account", "name\u{1b}[2J\n\t\u{202e}tail".into())],
        latest: "100.0%".into(),
        history: "100.0%".into(),
    };
    for width in [0, 1, 9, 40, 80, 120] {
        let lines = snapshot.lines(width);
        let joined = lines.concat();
        assert!(!joined.contains('\u{1b}') && !joined.contains('\u{202e}'));
        assert!(joined.contains("\\u{1b}[2J\\n\\t\\u{202e}tail"));
        assert!(
            lines
                .iter()
                .all(|line| crate::cells::cells(line) <= width.max(1)),
            "{width}: {lines:?}"
        );
        assert!(crate::cells::cells(&snapshot.compact(width)) <= width);
    }
    assert!(snapshot.compact(40).contains("F4"));
    assert!(snapshot.compact(40).contains("session 100.0%"));
}
