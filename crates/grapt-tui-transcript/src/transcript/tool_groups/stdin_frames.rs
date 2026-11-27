use super::*;
use grapt_client::{wire, Bounds, SessionView};
use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn fixture() -> std::result::Result<Vec<Slice>, Box<dyn std::error::Error>> {
    let mut items = vec![wire::MessageItem::Tool(wire::DurableTool {
        id: wire::tool_call_id("call_exec_fixture")?,
        name: "exec_command".into(),
        duration_ms: Some(250),
        progress: vec![],
        state: wire::DurableToolState::Completed {
            input: r#"{"cmd":"cargo test","description":"Watch the build"}"#.into(),
            output: r#"{"session_id":"operation_fixture","output":"build running"}"#.into(),
        },
    })];
    for (index, chars) in ["", "", "", "yes\n", "\u{3}"].into_iter().enumerate() {
        items.push(wire::MessageItem::Tool(wire::DurableTool {
            id: wire::tool_call_id(&format!("call_stdin_fixture_{index}"))?,
            name: "write_stdin".into(),
            duration_ms: Some(1500),
            progress: vec![],
            state: wire::DurableToolState::Completed {
                input: serde_json::json!({"session_id":"operation_fixture","chars":chars})
                    .to_string(),
                output: r#"{"output":"kept terminal output"}"#.into(),
            },
        }));
    }
    let message = wire::assistant_message(
        "msg_stdin_fixture",
        &items,
        Some(wire::DurableFinish::Stop),
        None,
        None,
    )?;
    let page = wire::decode_messages(&wire::encode_messages(vec![message], None)?)?;
    let mut view = SessionView::new(&wire::session_id("ses_stdin_fixture")?);
    view.apply_durable(&page.messages[0], Bounds::PRODUCTION)?;
    Ok(view.slices)
}

#[test]
fn wait_and_write_fixture_frames_keep_their_single_and_group_wording() -> Result {
    for light in [false, true] {
        let palette = Palette::new(light);
        let mut transcript = Transcript::new();
        transcript.update(true, 0, fixture()?);
        let folded = transcript.visible(120, 30, palette);
        let text = folded
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("waited on output 3 times, wrote input 2 times"),
            "{text}"
        );
        assert!(!text.to_lowercase().contains("sent input"));
        transcript.toggle_group(0);
        transcript.toggle_tool(1);
        let rows = transcript.visible(120, 40, palette);
        let mut terminal = Terminal::new(TestBackend::new(120, 40))?;
        terminal.draw(|frame| frame.render_widget(Paragraph::new(rows.clone()), frame.area()))?;
        let frame = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            frame.contains("Waited for Watch the build output · 1.5s"),
            "{frame}"
        );
        assert!(frame.contains("Wrote to Watch the build · 1.5s"), "{frame}");
        assert!(frame.contains("kept terminal output"));
        assert!(!frame.to_lowercase().contains("sent input"));
    }
    Ok(())
}
