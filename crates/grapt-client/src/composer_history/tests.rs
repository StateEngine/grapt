use super::{History, SearchStatus, MAX_BYTES, MAX_ENTRIES, MAX_QUERY_BYTES};

fn history() -> History {
    let mut history = History::default();
    history.refresh(["same", "newest unique", "same", "oldest unique"].into_iter());
    history
}

#[test]
fn recall_preserves_duplicates_and_down_restores_the_empty_draft() {
    let mut history = history();
    let Some(newest) = history.older("", 0) else {
        panic!("newest recall")
    };
    assert_eq!(newest.text, "same");
    assert!(history.older("unsent draft", 0).is_none());
    assert!(!history.should_navigate(&newest.text, 1));
    let Some(older) = history.older(&newest.text, newest.cursor) else {
        panic!("older recall")
    };
    assert_eq!(older.text, "newest unique");
    let Some(newer) = history.newer(&older.text, older.cursor) else {
        panic!("newer recall")
    };
    assert_eq!(newer, newest);
    let Some(original) = history.newer(&newer.text, newer.cursor) else {
        panic!("original draft")
    };
    assert!(original.text.is_empty());
    assert!(history.newer("", 0).is_none());
}

#[test]
fn edits_detach_navigation_and_do_not_replace_the_edited_prompt() {
    let mut history = history();
    assert!(history.older("", 0).is_some());
    history.edited();
    assert!(!history.should_navigate("edited", 0));
    assert!(history.newer("edited", 0).is_none());
}

#[test]
fn reverse_search_is_unique_and_boundaries_do_not_advance_hidden_selection() {
    let mut history = history();
    assert!(history.begin_search("draft", 2).is_some());
    assert!(history.set_query("same".to_owned()));
    assert_eq!(
        history.selected().map(|draft| draft.text),
        Some("same".to_owned())
    );
    assert_eq!(
        history.search_older().map(|draft| draft.text),
        Some("same".to_owned())
    );
    assert_eq!(history.status(), Some(SearchStatus::Boundary));
    assert_eq!(
        history.search_older().map(|draft| draft.text),
        Some("same".to_owned())
    );
    assert!(history.set_query("unique".to_owned()));
    assert_eq!(
        history.selected().map(|draft| draft.text),
        Some("newest unique".to_owned())
    );
    assert_eq!(
        history.search_older().map(|draft| draft.text),
        Some("oldest unique".to_owned())
    );
    assert_eq!(
        history.search_older().map(|draft| draft.text),
        Some("oldest unique".to_owned())
    );
    let Some(original) = history.cancel() else {
        panic!("cancel original")
    };
    assert_eq!(original.text, "draft");
    assert_eq!(original.cursor, 2);
}

#[test]
fn query_miss_and_oversize_query_leave_the_original_draft_recoverable() {
    let mut history = history();
    history.begin_search("unsent界", 3);
    assert!(history.set_query("absent".to_owned()));
    assert_eq!(history.status(), Some(SearchStatus::NoMatch));
    assert!(history.selected().is_none());
    assert!(!history.set_query("x".repeat(MAX_QUERY_BYTES + 1)));
    assert_eq!(history.query(), Some("absent"));
    let Some(original) = history.accept_search() else {
        panic!("accept miss restores draft")
    };
    assert_eq!(original.text, "unsent界");
    assert_eq!(original.cursor, 3);
}

#[test]
fn active_preview_freezes_history_and_session_cancel_restores_before_refresh() {
    let mut history = history();
    history.begin_search("original", 4);
    history.refresh(["another session"].into_iter());
    assert_eq!(
        history.selected().map(|draft| draft.text),
        Some("same".to_owned())
    );
    assert_eq!(
        history.cancel().map(|draft| draft.text),
        Some("original".to_owned())
    );
    history.refresh(["another session"].into_iter());
    assert_eq!(
        history.older("", 0).map(|draft| draft.text),
        Some("another session".to_owned())
    );
}

#[test]
fn history_limits_drop_whole_entries_and_report_omissions() {
    let mut history = History::default();
    let oversized = "界".repeat(MAX_BYTES / 3 + 1);
    history.refresh(
        std::iter::once(oversized.as_str()).chain(std::iter::repeat_n("kept", MAX_ENTRIES + 2)),
    );
    assert_eq!(history.omitted(), 3);
    assert_eq!(history.entries.len(), MAX_ENTRIES);
    assert!(history.entries.iter().all(|entry| entry == "kept"));
}

#[test]
fn reverse_search_uses_unicode_lowercase_and_newer_traversal_keeps_exact_text() {
    let mut history = History::default();
    history.refresh(["ÉCOLE", "école", "other École"].into_iter());
    history.begin_search("draft", 2);
    assert!(history.set_query("éCoLe".to_owned()));
    assert_eq!(
        history.selected().map(|draft| draft.text),
        Some("ÉCOLE".to_owned())
    );
    assert_eq!(
        history.search_older().map(|draft| draft.text),
        Some("école".to_owned())
    );
    assert_eq!(
        history.search_newer().map(|draft| draft.text),
        Some("ÉCOLE".to_owned())
    );
    assert_eq!(
        history.accept_search().map(|draft| draft.text),
        Some("ÉCOLE".to_owned())
    );
    assert!(!history.searching());
}
