use super::*;
use crate::transcript::Transcript;
use grapt_client::{wire, Bounds, SessionView, SliceKind};
use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};
use serde_json::json;

fn assistant(
    index: usize,
    finish: Option<wire::DurableFinish>,
    created: u64,
    completed: Option<u64>,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, Box<dyn std::error::Error>> {
    let dto =
        wire::assistant_message(&format!("msg_{index:012}"), &[], finish, Some((1, 2)), None)?;
    let mut value = serde_json::to_value(dto)?;
    value["time"] = json!({"created":created});
    if let Some(completed) = completed {
        value["time"]["completed"] = completed.into();
    }
    value["observedUsage"] = json!({"inputTotal":7545,"cacheRead":7065,"output":139});
    value["metadata"] = json!({"estimatedCost":{
        "status":"estimated","usd":0.00023575,"policy":"pinned",
        "scope":"assistant-message-api-equivalent"
    }});
    Ok(grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
        serde_json::from_value(value)?,
    )))
}

fn fold(
    rows: Vec<grapt_api::u3a::SessionMessageInfoDto>,
) -> Result<SessionView, Box<dyn std::error::Error>> {
    let bytes = wire::encode_messages(rows, None)?;
    let page = wire::decode_messages(&bytes)?;
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    for message in &page.messages {
        view.apply_durable(message, Bounds::PRODUCTION)?;
    }
    Ok(view)
}

fn footer(view: &SessionView) -> Result<&AssistantFooter, Box<dyn std::error::Error>> {
    view.slices
        .iter()
        .find_map(|slice| slice.footer.as_deref())
        .ok_or_else(|| "completed footer missing".into())
}

#[test]
fn ordered_public_reply_content_precedes_its_single_usage_footer(
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep tagged content in the typed codec. Generic Value maps sort "text"
    // before "type", which is not the public discriminant-first wire format.
    let view = fold(vec![wire::assistant_message(
        "msg_000000000001",
        &[wire::MessageItem::Text("The completed reply".into())],
        Some(wire::DurableFinish::Stop),
        Some((12, 7)),
        None,
    )?])?;
    let mut transcript = Transcript::new();
    transcript.update(true, 0, view.slices);
    let rows = transcript.visible(110, 20, Palette::new(false));
    let text: Vec<_> = rows.iter().map(ToString::to_string).collect();
    let reply = text
        .iter()
        .position(|row| row.contains("The completed reply"))
        .ok_or("reply absent")?;
    let footer = text
        .iter()
        .position(|row| row.contains("p1/m1 · input 12 · output 7"))
        .ok_or("usage absent")?;
    assert!(reply < footer);
    assert_eq!(
        text.iter().filter(|row| row.contains("API est.")).count(),
        1
    );
    Ok(())
}

#[test]
fn public_dto_to_transcript_retains_completion_and_usage() -> Result<(), Box<dyn std::error::Error>>
{
    let view = fold(vec![
        wire::user_message("msg_000000000001", "Question")?,
        assistant(2, Some(wire::DurableFinish::Stop), 1000, Some(4000))?,
    ])?;
    let info = footer(&view)?;
    assert_eq!(info.execution_started_ms, Some(1000));
    assert_eq!(info.completed_ms, Some(4000));
    // Frozen AssistantFooter: provider/id fallback, middle dots, three spaces,
    // text.subdued. TurnTokenUsage uses locale-grouped counts.
    let expected = "   Worked for 3s · done 00:00 UTC · p1/m1 · input 7.5k · output 139 · cache 93.6% · API est. $0.000236";
    for light in [false, true] {
        let palette = Palette::new(light);
        let mut transcript = Transcript::new();
        transcript.update(true, 0, view.slices.clone());
        let mut actual = Terminal::new(TestBackend::new(110, 30))?;
        actual.draw(|frame| transcript.paint(frame, frame.area(), palette))?;
        let buffer = actual.backend().buffer();
        let row = (0..30)
            .find(|y| {
                (0..110)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .starts_with(expected)
            })
            .ok_or("footer text missing from real transcript")?;
        assert_eq!(buffer[(3, row)].fg, palette.muted);
        assert_eq!(buffer[(3, row)].bg, palette.background);
        assert!(!buffer[(3, row)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD));
        // Re-listing reconciles the same typed assistant, without duplicates.
        transcript.reconcile(view.slices.clone());
        actual.draw(|frame| transcript.paint(frame, frame.area(), palette))?;
        let count = (0..30)
            .filter(|y| {
                (0..110)
                    .map(|x| actual.backend().buffer()[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("API est.")
            })
            .count();
        assert_eq!(count, 1);
    }
    Ok(())
}

#[test]
fn completion_span_shares_one_wrapping_footer_without_a_heading(
) -> Result<(), Box<dyn std::error::Error>> {
    let view = fold(vec![assistant(
        1,
        Some(wire::DurableFinish::Stop),
        1,
        Some(4),
    )?])?;
    let palette = Palette::new(false);
    for width in [1, 4, 28, 160] {
        let rows = lines(
            footer(&view)?,
            width,
            palette,
            Some(Span::styled(
                "Worked for 1m 2s · done 13:24",
                palette.normal().fg(palette.muted),
            )),
        );
        let mut terminal = Terminal::new(TestBackend::new(width, rows.len() as u16))?;
        terminal.draw(|frame| frame.render_widget(Paragraph::new(rows.clone()), frame.area()))?;
        let text = rows
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .skip(1)
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<String>();
        assert!(text.contains("Worked for 1m 2s · done 13:24 · p1/m1"));
        assert!(text.contains("API est. $0.000236"));
        assert!(!text.contains("Grapt"));
        assert!(rows.iter().all(|line| line.width() <= usize::from(width)));
    }
    Ok(())
}

#[test]
fn unknowns_are_not_zero_and_zero_input_has_no_cache_percentage(
) -> Result<(), Box<dyn std::error::Error>> {
    let view = fold(vec![assistant(
        1,
        Some(wire::DurableFinish::Stop),
        1,
        Some(4),
    )?])?;
    let mut info = footer(&view)?.clone();
    info.input = Some(0);
    info.output = Some(0);
    info.cache_read = Some(0);
    info.estimated_usd = Some(0.0);
    let text = grapt_client::view::assistant_footer::text(&info);
    assert!(text.contains("input 0 · output 0 · cache — · API est. $0.000000"));
    info.input = None;
    info.output = None;
    info.cache_read = None;
    info.estimated_usd = None;
    assert!(grapt_client::view::assistant_footer::text(&info)
        .ends_with("input — · output — · cache — · API est. —"));
    info.estimated_usd = Some(0.0000001);
    assert!(grapt_client::view::assistant_footer::text(&info).ends_with("API est. <$0.000001"));
    let missing = fold(vec![wire::assistant_message(
        "msg_000000000002",
        &[],
        Some(wire::DurableFinish::Stop),
        None,
        None,
    )?])?;
    let missing = footer(&missing)?;
    assert_eq!(
        (
            missing.input,
            missing.output,
            missing.cache_read,
            missing.estimated_usd
        ),
        (None, None, None, None)
    );
    Ok(())
}

#[test]
fn switching_history_preserves_each_reply_model_and_usage() -> Result<(), Box<dyn std::error::Error>>
{
    let first = assistant(1, Some(wire::DurableFinish::Stop), 1, Some(2))?;
    let second = assistant(2, Some(wire::DurableFinish::Stop), 3, None)?;
    let mut changed = serde_json::to_value(second)?;
    changed["model"]["id"] = "different-model".into();
    changed["model"]["providerID"] = "different-provider".into();
    let second = grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
        serde_json::from_value(changed)?,
    ));
    let view = fold(vec![first, second])?;
    let footers: Vec<_> = view
        .slices
        .iter()
        .filter_map(|s| s.footer.as_deref())
        .collect();
    assert_eq!(footers.len(), 2);
    assert!(grapt_client::view::assistant_footer::text(footers[0]).starts_with("p1/m1 ·"));
    assert!(grapt_client::view::assistant_footer::text(footers[1])
        .starts_with("different-provider/different-model ·"));
    assert_eq!(footers[1].completed_ms, None);
    assert_ne!(footers[0].message, footers[1].message);
    Ok(())
}

#[test]
fn timing_spans_tool_steps_only_after_a_known_input_and_never_reuses_an_old_execution(
) -> Result<(), Box<dyn std::error::Error>> {
    let tool = assistant(2, Some(wire::DurableFinish::ToolCalls), 1000, Some(2000))?;
    let done = assistant(3, Some(wire::DurableFinish::Stop), 3000, Some(4000))?;
    let view = fold(vec![
        wire::user_message("msg_000000000001", "Question")?,
        tool.clone(),
        done.clone(),
    ])?;
    assert_eq!(
        view.slices
            .iter()
            .filter(|s| s.kind == SliceKind::AssistantFooter)
            .count(),
        1
    );
    assert_eq!(footer(&view)?.execution_started_ms, Some(1000));
    assert_eq!(footer(&view)?.created_ms, Some(3000));
    assert_eq!(footer(&fold(vec![tool, done])?)?.execution_started_ms, None);
    let view = fold(vec![
        wire::user_message("msg_000000000001", "Question")?,
        assistant(2, Some(wire::DurableFinish::Stop), 1000, Some(2000))?,
        assistant(3, Some(wire::DurableFinish::Stop), 3000, Some(4000))?,
    ])?;
    assert_eq!(
        view.slices
            .last()
            .and_then(|s| s.footer.as_deref())
            .ok_or("last missing")?
            .execution_started_ms,
        None
    );
    for finish in [
        None,
        Some(wire::DurableFinish::Unknown),
        Some(wire::DurableFinish::ToolCalls),
    ] {
        let view = fold(vec![assistant(1, finish, 1, Some(2))?])?;
        assert!(!view
            .slices
            .iter()
            .any(|s| s.kind == SliceKind::AssistantFooter));
    }
    Ok(())
}

#[test]
fn unpaired_cache_reads_never_borrow_the_legacy_input_denominator(
) -> Result<(), Box<dyn std::error::Error>> {
    for read in [0, 50] {
        let message = wire::assistant_message(
            "msg_000000000001",
            &[],
            Some(wire::DurableFinish::Stop),
            Some((100, 2)),
            None,
        )?;
        let mut value = serde_json::to_value(message)?;
        value["observedUsage"] = json!({"cacheRead":read});
        let message = grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
            serde_json::from_value(value)?,
        ));
        let view = fold(vec![message])?;
        let text = grapt_client::view::assistant_footer::text(footer(&view)?);
        assert!(text.contains("input 100 · output 2 · cache —"), "{text}");
    }
    Ok(())
}

#[test]
fn reply_footer_stays_visible_between_distinct_folded_tool_runs(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut messages = Vec::new();
    for (index, finish) in [
        (1, wire::DurableFinish::Stop),
        (2, wire::DurableFinish::ToolCalls),
    ] {
        let tool = wire::DurableTool {
            duration_ms: None,
            id: wire::tool_call_id(&format!("call_{index}"))?,
            name: "shell".into(),
            state: wire::DurableToolState::Completed {
                input: "{}".into(),
                output: "retained command output".into(),
            },
            progress: Vec::new(),
        };
        messages.push(wire::assistant_message(
            &format!("msg_{index:012}"),
            &[wire::MessageItem::Tool(tool)],
            Some(finish),
            Some((12, 7)),
            None,
        )?);
    }
    let view = fold(messages)?;
    let mut transcript = Transcript::new();
    transcript.update(true, 0, view.slices.clone());
    for reconcile in [false, true] {
        if reconcile {
            transcript.reconcile(view.slices.clone());
        }
        let rows = transcript.visible(120, 40, Palette::new(false));
        let text: Vec<_> = rows.iter().map(ToString::to_string).collect();
        let tools: Vec<_> = text
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.contains("Ran command").then_some(index))
            .collect();
        let usage: Vec<_> = text
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.contains("API est.").then_some(index))
            .collect();
        assert_eq!(tools.len(), 2);
        assert_eq!(usage.len(), 1);
        assert!(tools[0] < usage[0] && usage[0] < tools[1]);
    }
    Ok(())
}

#[test]
fn completion_uses_execution_time_in_the_single_wrapped_reply_footer(
) -> Result<(), Box<dyn std::error::Error>> {
    let view = fold(vec![
        wire::user_message("msg_000000000001", "Question")?,
        assistant(
            2,
            Some(wire::DurableFinish::ToolCalls),
            10_000,
            Some(15_000),
        )?,
        assistant(3, Some(wire::DurableFinish::Stop), 20_000, Some(75_000))?,
    ])?;
    let mut transcript = Transcript::new();
    transcript.update(true, 0, view.slices.clone());
    for width in [120, 45] {
        for reconcile in [false, true] {
            if reconcile {
                transcript.reconcile(view.slices.clone());
            }
            let rows = transcript.visible(width, 40, Palette::new(false));
            let text = rows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.matches("Worked for 1m 5s").count(), 1, "{text}");
            assert!(text.contains("done 00:01 UTC"), "{text}");
            assert_eq!(text.matches("API est.").count(), 1, "{text}");
            assert!(rows.iter().all(|row| row.width() <= usize::from(width)));
        }
    }
    let view = fold(vec![assistant(
        4,
        Some(wire::DurableFinish::Stop),
        20_000,
        Some(75_000),
    )?])?;
    let unknown = footer(&view)?;
    assert_eq!(
        completion(unknown, Palette::new(false)).map(|span| span.content.into_owned()),
        Some("done 00:01 UTC".into())
    );
    let view = fold(vec![assistant(
        5,
        Some(wire::DurableFinish::Stop),
        20_000,
        None,
    )?])?;
    assert!(completion(footer(&view)?, Palette::new(false)).is_none());
    Ok(())
}

#[test]
fn audit_interrupted_footer_keeps_its_outcome_after_render_and_reconcile(
) -> Result<(), Box<dyn std::error::Error>> {
    // The audit's e819444e run-04 history and frame 15-after-escape, reduced to
    // the interrupted exchange and footer row. Only the incorrect label changes.
    let page = wire::decode_messages(include_bytes!("audit-interrupted.json"))?;
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    for message in &page.messages {
        view.apply_durable(message, Bounds::PRODUCTION)?;
    }
    assert_eq!(
        footer(&view)?.outcome,
        Some(grapt_client::Outcome::Interrupted)
    );
    let expected = include_str!("audit-interrupted-footer.txt")
        .trim()
        .replace(" · done ", " · interrupted ");
    let mut transcript = Transcript::new();
    transcript.update(true, 0, view.slices.clone());
    for light in [false, true] {
        let palette = Palette::new(light);
        for width in [120, 45, 28] {
            for reconcile in [false, true] {
                if reconcile {
                    transcript.reconcile(view.slices.clone());
                }
                let rows = transcript.visible(width, 40, palette);
                let joined = rows
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert_eq!(joined.matches("Worked for 1s").count(), 1, "{joined}");
                assert!(!joined.contains(" · done "), "{joined}");
                let rendered_footer = reply(footer(&view)?, width, palette);
                let flat = rendered_footer
                    .iter()
                    .flat_map(|line| line.spans.iter().skip(1))
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert_eq!(flat, expected);
                assert!(rows.iter().all(|row| row.width() <= usize::from(width)));
                let mut terminal = Terminal::new(TestBackend::new(width, 40))?;
                terminal.draw(|frame| transcript.paint(frame, frame.area(), palette))?;
                let buffer = terminal.backend().buffer();
                for (y, line) in rows.iter().enumerate() {
                    let painted = (0..width)
                        .map(|x| buffer[(x, y as u16)].symbol())
                        .collect::<String>();
                    assert_eq!(painted.trim_end(), line.to_string().trim_end());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn public_terminal_not_usage_selects_the_footer_label() -> Result<(), Box<dyn std::error::Error>> {
    for (finish, error, expected) in [
        (None, Some(("aborted", "Step interrupted")), "interrupted"),
        (None, Some(("aborted", "Permission declined")), "failed"),
        (None, Some(("other", "Step interrupted")), "failed"),
        (Some("stop"), None, "done"),
        (Some("error"), None, "failed"),
    ] {
        for known_usage in [false, true] {
            let mut value: serde_json::Value =
                serde_json::from_str(include_str!("audit-interrupted.json"))?;
            let message = &mut value["data"][1];
            message
                .as_object_mut()
                .ok_or("assistant object")?
                .remove("error");
            if let Some(finish) = finish {
                message["finish"] = finish.into();
            }
            if let Some((kind, text)) = error {
                message["error"] = json!({"type":kind, "message":text});
            }
            if known_usage {
                message["observedUsage"] = json!({"inputTotal":100, "cacheRead":50, "output":7});
            }
            // Generic Value maps reorder the public union discriminator. Encode
            // typed DTOs, as the server does, before testing the real decoder.
            let view = fold(vec![
                wire::user_message("msg_000000000001", "Review the change")?,
                grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(serde_json::from_value(
                    message.clone(),
                )?)),
            ])?;
            let rows = reply(footer(&view)?, 160, Palette::new(false));
            let text = rows.iter().map(ToString::to_string).collect::<String>();
            assert!(text.contains(&format!("Worked for 1s · {expected} 21:48 UTC")));
            assert!(text.contains(if known_usage {
                "input 100 · output 7 · cache 50.0%"
            } else {
                "input — · output — · cache —"
            }));
        }
    }
    Ok(())
}
