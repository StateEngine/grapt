use crate::{backend::Backend, keys, ui::Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{wire::*, Bounds, SessionView};
use ratatui::{backend::TestBackend, Terminal};
use std::{sync::mpsc, time::Duration};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn backend() -> Backend {
    let (commands, _) = mpsc::channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    Backend {
        commands,
        updates,
        wake,
    }
}
fn rows(status: &str, id: &str) -> Result<Vec<grapt_client::Slice>> {
    let fields = if status == "failed" {
        r#""error":{"type":"provider","message":"Compaction account refused"}"#
    } else {
        r#""summary":"Full retained summary.\nSecond paragraph.","recent":"msg_kept""#
    };
    let payload = format!(
        r#"{{"data":[{{"type":"compaction","status":"{status}","id":"{id}","time":{{"created":1}},"reason":"manual","metadata":{{"tokensBefore":1200,"tokensAfter":350,"trigger":"manual"}},{fields}}}],"cursor":{{}}}}"#
    );
    let page = decode_messages(payload.as_bytes())?;
    let mut view = SessionView::new(&session_id("ses_compaction_fold")?);
    for message in page.messages {
        view.apply_durable(&message, Bounds::default())?;
    }
    Ok(view.slices)
}
#[test]
fn compaction_disclosure_uses_diff_controls_and_retains_identity_across_replay() -> Result {
    for light in [false, true] {
        let backend = backend();
        let mut ui = Ui::new(light);
        ui.composer.insert_str("keep compaction draft");
        let source = rows("completed", "msg_compaction_one")?;
        assert!(source[0].text.contains("msg_kept") && source[0].text.contains("tokensBefore"));
        ui.transcript.update(true, 0, source.clone());
        assert!(!ui.transcript.cell_expanded(0));
        let mut terminal = Terminal::new(TestBackend::new(110, 30))?;
        terminal.draw(|f| ui.draw(f))?;
        let hit = ui
            .transcript
            .painted_rows()
            .enumerate()
            .find_map(|(y, row)| {
                (row.row == 0 && ui.transcript.cell_slice(row.cell).compaction.is_some())
                    .then_some((ui.transcript_area.x, ui.transcript_area.y + y as u16))
            })
            .ok_or("compaction disclosure target missing")?;
        keys::handle_key(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &mut ui,
            &backend,
        )?;
        terminal.draw(|f| ui.draw(f))?;
        // A real click arrives as press/release without a repaint between.
        // Keyboard focus must not consume the first disclosure click.
        for kind in [
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        ] {
            assert!(crate::mouse::handle(
                crossterm::event::MouseEvent {
                    kind,
                    column: hit.0,
                    row: hit.1,
                    modifiers: KeyModifiers::NONE,
                },
                &mut ui,
            ));
        }
        assert!(ui.transcript.cell_expanded(0));
        let mut prefix = source[0].clone();
        prefix.compaction = None;
        prefix.text = "New earlier session notice".into();
        ui.transcript.reconcile(vec![prefix, source[0].clone()]);
        assert!(ui.transcript.cell_expanded(1));
        terminal.draw(|f| ui.draw(f))?;
        keys::handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
        )?;
        assert!(!ui.transcript.cell_expanded(1));
        terminal.draw(|f| ui.draw(f))?;
        keys::handle_key(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &mut ui,
            &backend,
        )?;
        keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(ui.transcript.cell_expanded(1));
        assert_eq!(keys::draft(&ui), "keep compaction draft");
        reopened_turn_resurfaces_only_current_compaction(&mut ui)?;
        ui.transcript
            .update(true, 0, rows("failed", "msg_compaction_failed")?);
        assert!(ui
            .transcript
            .cell_slice(0)
            .text
            .contains("Compaction account refused"));
        assert_eq!(keys::draft(&ui), "keep compaction draft");
        let retained = ui.transcript.recent_rows(16);
        assert!(ui.transcript.observe_compacting(true));
        assert!(!ui.transcript.observe_compacting(true));
        terminal.draw(|f| ui.draw(f))?;
        assert_eq!(
            ui.transcript.recent_rows(16),
            retained,
            "a live phase cannot fabricate a durable history row"
        );
        assert!(ui.transcript.observe_compacting(false));
    }
    Ok(())
}

/// A reopened session replays an interrupted compaction as history. A new turn
/// must not resurrect its banner, while a compaction that starts in that turn
/// must still show.
fn reopened_turn_resurfaces_only_current_compaction(ui: &mut Ui) -> Result {
    ui.transcript
        .update(true, 0, rows("running", "msg_compaction_live")?);
    // A compaction cell only counts while its turn is live; a replayed
    // `Running` row from a crashed turn must not say "Compacting…".
    assert!(
        !ui.transcript.compacting(),
        "no live turn means no current compaction"
    );
    // Reopen and send an ordinary prompt: the replayed call predates the new
    // turn, so its banner must not come back. The durable replay and the turn
    // start arrive in one batch and can share a clock tick, which is exactly
    // the case a time comparison gets wrong.
    ui.transcript.observe_running(true);
    assert!(
        !ui.transcript.compacting(),
        "a compaction replayed before this turn is not current, even at the same tick"
    );
    // A compaction that actually starts in this turn still shows.
    ui.transcript
        .update(false, 1, rows("running", "msg_compaction_current")?);
    assert!(
        ui.transcript.compacting(),
        "a compaction observed in this turn is current"
    );
    assert!(!ui.transcript.cell_slice(0).closed);
    assert!(ui.transcript.tick_compactions(Duration::from_secs(14)));
    assert!(
        ui.transcript
            .cell_slice(1)
            .text
            .find("Full retained summary")
            .is_none(),
        "running summaries stay provisional"
    );
    ui.transcript
        .update(false, 1, rows("completed", "msg_compaction_current")?);
    assert!(
        !ui.transcript.compacting(),
        "finishing the current compaction clears the banner"
    );
    ui.transcript
        .update(false, 0, rows("completed", "msg_compaction_live")?);
    assert!(!ui.transcript.cell_expanded(0));
    Ok(())
}

/// A completed compaction that reports both estimates says what it did on its
/// heading line; one that reports neither keeps the plain heading.
#[test]
fn a_completed_compaction_with_estimates_says_what_it_did() -> Result {
    for (estimates, heading) in [
        (
            r#","metadata":{"tokenEstimates":{"before":"1200","after":"350","basis":"estimated"}}"#,
            "Context compacted: 1200 → 350 tokens.",
        ),
        ("", "Compaction completed."),
    ] {
        let payload = format!(
            r#"{{"data":[{{"type":"compaction","status":"completed","id":"msg_compaction_sized","time":{{"created":1}},"reason":"auto"{estimates},"summary":"Retained.","recent":"msg_kept"}}],"cursor":{{}}}}"#
        );
        let page = decode_messages(payload.as_bytes())?;
        let mut view = SessionView::new(&session_id("ses_compaction_sized")?);
        for message in page.messages {
            view.apply_durable(&message, Bounds::default())?;
        }
        let mut ui = Ui::new(false);
        ui.transcript.update(true, 0, view.slices);
        let slice = ui.transcript.cell_slice(0);
        assert!(slice.compaction.is_some(), "the row is not a compaction");
        assert_eq!(
            slice.text.lines().next(),
            Some(heading),
            "the heading is not {heading:?}"
        );
    }
    Ok(())
}
