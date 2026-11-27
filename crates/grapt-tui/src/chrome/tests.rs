use super::*;
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn live_activity_is_visible_above_the_prompt_and_clears_at_completion() -> Result {
    let mut ui = Ui::new(false);
    let now = tokio::time::Instant::now();
    ui.activity
        .observe(true, Some(3600), crate::activity::Phase::Thinking, now);
    ui.activity.tick(now + std::time::Duration::from_secs(103));
    for width in [40, 80, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24))?;
        terminal.draw(|frame| ui.draw(frame))?;
        assert!(line(terminal.backend().buffer(), ui.composer_area.y)
            .contains("Working (1m 43s · ↓ ~3.6k tokens)"));
        assert!(!footer(&ui, usize::from(width))[0].contains("Working"));
    }
    ui.activity
        .observe(false, None, crate::activity::Phase::Thinking, now);
    assert!(!ui.activity.active());
    Ok(())
}

#[test]
fn bounded_labels_and_title_fallback_preserve_explicit_titles() {
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.prompt_ready = true;
    for width in 0..=160 {
        let rows = footer(&ui, width);
        assert!(rows.iter().all(|row| cells::cells(row) <= width));
    }
    assert_eq!(
        title("[seat-runner] You are Ada. Seat serial: example-serial"),
        "Ada · seat"
    );
    assert_eq!(title("My explicit title"), "My explicit title");
    assert_eq!(fit("A中👩‍💻very long title", 7), "A中👩‍💻v…");
}

#[test]
fn idle_footer_is_sparse_and_workspace_snapshot_preserves_the_path_tail() -> Result {
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.prompt_ready = true;
    assert!(ui.workspace_update(Some("/home/example/projects/中/my-workspace")));
    assert!(!ui.workspace_update(Some("/home/example/projects/中/my-workspace")));
    for width in 0..=160 {
        let rows = footer(&ui, width);
        assert!(rows.iter().all(|row| cells::cells(row) <= width));
        assert!(!rows[0].contains("F6 questions (0)"));
        assert!(!rows[0].contains("ctrl+p") && !rows[0].contains("F3 model"));
        assert!(!rows[1].contains("live tail"));
    }
    ui.repository_status = Some("Repository unavailable: permission denied".into());
    assert!(footer(&ui, 80)
        .iter()
        .any(|row| row.contains("permission denied")));
    assert!(ui.workspace_update(None));
    assert_eq!(ui.directory, None);
    Ok(())
}

#[test]
fn offline_mail_keeps_priority_over_hints_and_history_recall() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.prompt_ready = true;
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::Unavailable);
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::ZERO);
    ui.mail_recovery
        .advance(ui.mail_attention, std::time::Duration::from_secs(30));
    for width in 0..=160 {
        let rows = footer(&ui, width);
        assert!(rows.iter().all(|row| cells::cells(row) <= width));
        if width >= "mail offline".len() {
            assert!(rows[0].contains("offline"), "{}", rows[0]);
        }
    }
    assert!(ui.recall.key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &ui.transcript,
        &mut ui.composer,
    ));
    assert!(ui.recall.status().is_some(), "recall branch must be active");
    for width in [18, 24, 40, 80, 160] {
        let rows = footer(&ui, width);
        assert!(rows[0].contains("offline"), "{}", rows[0]);
        assert!(rows.iter().all(|row| cells::cells(row) <= width));
    }
    ui.mail_attention = None;
    let rows = footer(&ui, 160);
    assert!(!rows[0].contains("offline"));
    assert!(rows[0].contains("Loaded history"));
}

#[test]
fn routine_policy_is_in_details_and_only_read_failures_need_a_banner() {
    let mut ui = Ui::new(false);
    for policy in [
        "Preset: default | effective: maintainer",
        "Bypass permissions on | Preset: bypass | effective: bypass",
    ] {
        ui.policy = Some(policy.into());
        assert!(ui.policy_banner().is_none());
    }
    ui.policy =
        Some("Bypass permissions on (last confirmed) | Preset unavailable: disconnected".into());
    assert_eq!(ui.policy_banner(), Some("Preset unavailable: disconnected"));
}

fn line(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

#[test]
fn the_gap_notice_shows_in_the_footer_and_clears_with_the_fact() -> Result {
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    assert!(!footer(&ui, 80)[0].contains("durable history"));

    ui.gap_notice.advance(true);
    for width in [80, 43, 24] {
        let rows = footer(&ui, width);
        let text = &rows[0];
        // Narrow terminals truncate the notice; they must never drop it, which
        // is what the previous append-based row did below about 43 cells.
        assert!(
            text.starts_with("Live view refreshing"),
            "the notice leads the row at {width} cells: {text:?}"
        );
        assert!(cells::cells(text) <= width);
    }
    assert!(footer(&ui, 80)[0].contains("durable history"));

    // The hint and recall branches return early, so they are the paths where a
    // leading notice is easiest to lose; drive both through `leading` itself.
    let hint = crate::quit::HINT;
    for width in [80, 24] {
        let line = leading(Some("Live view refreshing"), hint, width);
        assert!(line.starts_with("Live view refreshing"), "{line:?}");
        assert!(cells::cells(&line) <= width, "{line:?}");
        assert_eq!(leading(None, hint, width), fit(hint, width));
    }
    let recall = "Loaded history match · Enter edit · Esc cancel";
    let line = leading(Some("Live view refreshing"), recall, 80);
    assert!(line.starts_with("Live view refreshing"), "{line:?}");
    assert!(line.contains("Loaded history"), "{line:?}");

    ui.gap_notice.advance(false);
    let rows = footer(&ui, 80);
    assert!(!rows[0].contains("durable history"), "{:?}", rows[0]);
    Ok(())
}

#[test]
fn context_pressure_uses_known_current_context_above_seventy_percent() {
    let mut context = crate::backend::Context::default();
    assert!(context_meter(&context).is_none());
    for (used, capacity, shown) in [
        (70, 100, false),
        (0, 100, false),
        (100, 0, false),
        (71, 100, true),
        (101, 100, true),
        (u64::MAX, u64::MAX, true),
    ] {
        context.used = Some(used);
        context.capacity = Some(capacity);
        assert_eq!(context_meter(&context).is_some(), shown);
    }
    context.error = Some("report unavailable".into());
    assert!(context_meter(&context).is_none());
}

#[test]
fn long_session_labels_preserve_model_and_effort_within_available_cells() -> Result {
    use grapt_client::wire::{decode_model_variant, model_id, provider_id, ModelRow};
    let mut ui = Ui::new(false);
    ui.workspace_update(Some("/project/very-long-workspace-name-中中中中中中"));
    ui.title = "[seat-runner] You are Long Agent Name. Seat serial: agt_example".into();
    ui.model = Some(ModelRow {
        id: model_id("gpt-5")?,
        provider_id: provider_id("openai")?,
        name: "GPT-5".into(),
        enabled: true,
        variant: decode_model_variant(
            br#"{"model":{"id":"gpt-5","providerID":"openai","variant":"high"}}"#,
        )?,
        context_window: Some(200_000),
        default_effort: None,
        capabilities: grapt_client::wire::ModelCapabilities {
            tools: Some(true),
            input_text: Some(true),
            output_text: Some(true),
        },
    });
    for width in [24, 40, 80] {
        let context = session_context(&ui, width);
        assert!(context.contains("gpt-5") && context.contains("high"));
        assert!(cells::cells(&context) <= width);
    }
    Ok(())
}

#[test]
fn degraded_continuity_is_a_status_line_state_beside_mail() {
    use grapt_api::u3a::{ContinuityAttentionInfo, ContinuityDegradedReason};
    let mut ui = Ui::new(false);
    ui.status = "Connected".into();
    ui.prompt_ready = true;
    for level in [None, Some(ContinuityAttentionInfo::Ok)] {
        ui.continuity_attention = level;
        assert!(!footer(&ui, 160)[0].contains("continuity"), "{level:?}");
    }
    ui.continuity_attention = Some(ContinuityAttentionInfo::Degraded {
        reason: ContinuityDegradedReason::Capability,
    });
    assert!(footer(&ui, 160)[0].contains("continuity: rebind"));
    ui.continuity_attention = Some(ContinuityAttentionInfo::Degraded {
        reason: ContinuityDegradedReason::Transport,
    });
    for width in 0..=160 {
        let rows = footer(&ui, width);
        assert!(rows.iter().all(|row| cells::cells(row) <= width));
        if width >= "continuity offline".len() + 2 {
            assert!(
                rows[0].contains("continuity offline"),
                "{width}: {}",
                rows[0]
            );
        }
    }
    ui.mail_attention = Some(grapt_api::u3a::MailAttentionInfo::RebindRequired);
    let row = &footer(&ui, 160)[0];
    assert!(row.contains("continuity offline · mail offline"), "{row}");
}
