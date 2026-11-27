use super::*;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use ratatui_textarea::CursorMove;

fn editor(text: &str) -> TextArea<'static> {
    let mut editor = TextArea::default();
    editor.insert_str(text);
    editor
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn cursor_local_unicode_completion_preserves_other_mentions_and_undo() -> Result<(), String> {
    let mut text = editor("界 @old\nread @srcSuffix then @other");
    text.move_cursor(CursorMove::Jump(1, 9));
    let token = Token::at(&text).ok_or("missing token")?;
    assert_eq!(token.query, "src");
    token.insert(&mut text, "src/a b中.rs")?;
    assert_eq!(
        text.lines(),
        ["界 @old", "read \"src/a b中.rs\"  then @other"]
    );
    assert!(text.undo());
    assert_eq!(text.lines(), ["界 @old", "read @srcSuffix then @other"]);
    assert_eq!(text.cursor(), ratatui_textarea::DataCursor(1, 9));
    assert!(text.redo());
    let completed = text.lines().to_vec();
    text.insert_str("X");
    assert!(text.undo());
    assert_eq!(text.lines(), completed);
    assert!(text.undo());
    assert_eq!(text.lines(), ["界 @old", "read @srcSuffix then @other"]);
    assert!(text.undo());
    assert_eq!(text.lines(), [""]);
    Ok(())
}

#[test]
fn email_controls_and_overlong_queries_are_not_mentions() {
    for text in ["mail user@example", "hello", "@x\u{1b}"] {
        assert!(Token::at(&editor(text)).is_none(), "{text:?}");
    }
    assert!(Token::at(&editor(&format!("@{}", "a".repeat(257)))).is_none());
    assert!(Token::at(&editor("@")).is_some());
}

#[test]
fn escape_dismissal_survives_refresh_until_token_changes() -> Result<(), String> {
    let mut picker = Mentions::default();
    let mut text = editor("@src");
    assert!(picker.sync(&text, true));
    assert!(picker.key(key(KeyCode::Esc), &mut text)?);
    assert!(!picker.visible());
    assert!(!picker.sync(&text, true));
    picker.sync(&text, false);
    assert!(!picker.sync(&text, true));
    assert!(
        !picker.visible(),
        "temporary disabling must preserve Escape dismissal"
    );
    text.insert_str("/");
    assert!(picker.sync(&text, true));
    assert!(picker.visible());
    assert!(picker.sync(&text, false));
    assert!(!picker.visible());
    Ok(())
}

#[test]
fn enter_and_tab_insert_only_a_selected_path_and_respect_draft_bound() -> Result<(), String> {
    for code in [KeyCode::Enter, KeyCode::Tab] {
        let mut picker = Mentions::default();
        let mut text = editor("look @ma");
        picker.sync(&text, true);
        assert!(picker.key(key(code), &mut text)?);
        assert_eq!(text.lines(), ["look @ma"]); // Never sends an unfinished query.
        picker.paths = vec!["src/main.rs".into()];
        assert!(picker.key(key(code), &mut text)?);
        assert_eq!(text.lines(), ["look src/main.rs "]);
        assert!(!picker.visible());
    }
    let mut text = editor(&format!("{} @x", "a".repeat(crate::MAX_DRAFT - 4)));
    let before = text.lines().to_vec();
    let token = Token::at(&text).ok_or("missing token")?;
    assert!(token.insert(&mut text, "too-long.rs").is_err());
    assert_eq!(text.lines(), before);
    Ok(())
}

#[test]
fn stale_results_and_old_workspace_never_replace_current_matches() -> Result<(), String> {
    let (jobs, _input) = std::sync::mpsc::sync_channel(1);
    let (output, replies) = std::sync::mpsc::sync_channel(1);
    let mut picker = Mentions {
        worker: Some(Worker { jobs, replies }),
        ..Mentions::default()
    };
    let directory = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = directory.join("workspace");
    let other = directory.join("other");
    picker.workspace(Some(root.to_str().ok_or("non-UTF-8 workspace")?));
    let mut text = editor("@old");
    picker.sync(&text, true);
    let old = picker.active.clone().ok_or("missing active")?;
    let stale = picker.queued.take().ok_or("missing search")?.request;
    text.insert_str("x");
    picker.sync(&text, true);
    output
        .try_send(worker::Reply {
            request: stale,
            token: old,
            root: root.clone(),
            result: Err("stale error".into()),
        })
        .map_err(|e| e.to_string())?;
    let stale_replacement = picker
        .queued
        .take()
        .ok_or("missing replacement search")?
        .request;
    picker.poll();
    assert!(!picker.note.contains("stale error"));
    let active = picker.active.clone().ok_or("missing active")?;
    let stale = stale_replacement;
    picker.workspace(Some(other.to_str().ok_or("non-UTF-8 workspace")?));
    picker.sync(&text, true);
    output
        .try_send(worker::Reply {
            request: stale,
            token: active,
            root: root.clone(),
            result: Err("wrong workspace".into()),
        })
        .map_err(|e| e.to_string())?;
    picker.poll();
    assert!(!picker.note.contains("wrong workspace"));
    Ok(())
}

#[test]
fn short_popup_keeps_selection_and_status_visible() -> Result<(), Box<dyn std::error::Error>> {
    let mut picker = Mentions::default();
    picker.sync(&editor("@src"), true);
    picker.paths = (0..20)
        .map(|i| PathBuf::from(format!("src/{i}.rs")))
        .collect();
    picker.selected = 19;
    picker.note = "Search limited".into();
    let mut terminal = Terminal::new(TestBackend::new(50, 8))?;
    terminal.draw(|frame| {
        picker.draw(
            frame,
            Rect::new(0, 5, 50, 3),
            grapt_tui_transcript::palette::Palette::new(false),
        )
    })?;
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("src/19.rs"));
    assert!(screen.contains("Search limited"));
    Ok(())
}

#[test]
fn workspace_search_worker_returns_an_ignored_filtered_path_that_tab_inserts(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Workspace(PathBuf);
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = std::env::temp_dir().join(format!(
        "grapt-r-mentions-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root)?;
    let workspace = Workspace(root);
    std::fs::create_dir(workspace.0.join("src"))?;
    std::fs::create_dir(workspace.0.join(".git"))?;
    std::fs::write(workspace.0.join(".gitignore"), "ignored-main.rs\n")?;
    std::fs::write(workspace.0.join("src/main.rs"), "fixture")?;
    std::fs::write(workspace.0.join("ignored-main.rs"), "ignored")?;
    let mut picker = Mentions::default();
    let (wake, notification) = std::sync::mpsc::sync_channel(1);
    picker.start(wake)?;
    picker.workspace(Some(workspace.0.to_str().ok_or("workspace path")?));
    let mut text = editor("read @main.rs");
    assert!(picker.sync(&text, true));
    picker.poll();
    for _ in 0..8 {
        notification.recv_timeout(Duration::from_secs(2))?;
        picker.poll();
        if !picker.paths.is_empty() {
            break;
        }
    }
    assert_eq!(picker.paths, [PathBuf::from("src/main.rs")]);
    let expected = format!("read {} ", picker.paths[0].display());
    assert!(picker.key(key(KeyCode::Tab), &mut text)?);
    assert_eq!(text.lines(), [expected]);
    assert!(!picker.visible());
    drop(picker);
    Ok(())
}

#[test]
fn canceled_search_releases_dispatch_capacity_with_a_wake() -> Result<(), String> {
    use std::{sync::mpsc, time::Duration};
    let (wake, notification) = mpsc::sync_channel(1);
    let worker = Worker::start(wake)?;
    let mut control = SearchController::default();
    let root = std::env::temp_dir().join("grapt-canceled-search-no-read");
    let request = control
        .begin(&root, "old", Limits::PRODUCTION)
        .map_err(|e| e.to_string())?;
    control.cancel();
    worker
        .jobs
        .send(Job {
            token: Token::at(&editor("@old")).ok_or("token")?,
            root,
            request,
        })
        .map_err(|e| e.to_string())?;
    notification
        .recv_timeout(Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    assert!(
        worker.replies.try_recv().is_err(),
        "canceled search cannot publish results"
    );
    Ok(())
}
