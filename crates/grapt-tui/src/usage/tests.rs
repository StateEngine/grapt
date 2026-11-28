use super::*;
use crate::{backend::Backend, keys, ui::Ui};
use crossterm::event::{KeyEvent, KeyModifiers};
use grapt_client::{
    double::ContractDouble,
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    view::status,
    Bounds, Client, Connection, Operation,
};
use ratatui::{backend::TestBackend, Terminal};
use std::sync::mpsc;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    rows: Vec<grapt_api::u3a::SessionMessageInfoDto>,
    partial: bool,
}
impl Peer {
    /// This peer's rows as one message page, newest first when asked.
    fn page(&self, descending: bool) -> std::result::Result<Vec<u8>, TransportError> {
        let mut rows = self.rows.clone();
        if descending {
            rows.reverse();
        }
        grapt_client::wire::encode_messages(rows, self.partial.then_some("older_cache_page"))
            .map_err(|_| TransportError::NotConnected)
    }
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> std::result::Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            let page = self.page(true)?;
            let body = grapt_client::double::page_snapshot(request, &page, 0)
                .map_err(|_| TransportError::NotConnected)?;
            return Ok(WireResponse { status: 200, body });
        }
        if request.operation() == Operation::MessageList {
            let descending = request
                .query()
                .iter()
                .any(|(k, v)| k == "order" && v == "desc");
            return Ok(WireResponse {
                status: 200,
                body: self.page(descending)?,
            });
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> std::result::Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe()
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}
fn row(
    index: u32,
    input: Option<u64>,
    read: Option<u64>,
    calls: bool,
) -> std::result::Result<grapt_api::u3a::SessionMessageInfoDto, Box<dyn std::error::Error>> {
    let row = grapt_client::wire::assistant_message(
        &format!("msg_{index:012}"),
        &[],
        Some(grapt_client::wire::DurableFinish::Stop),
        Some((7, 11)),
        None,
    )?;
    let mut value = serde_json::to_value(row)?;
    if input.is_some() || read.is_some() {
        value["observedUsage"] = serde_json::json!({"inputTotal":input,"cacheRead":read});
        if input.is_none() {
            value["observedUsage"]
                .as_object_mut()
                .ok_or("usage")?
                .remove("inputTotal");
        }
        if read.is_none() {
            value["observedUsage"]
                .as_object_mut()
                .ok_or("usage")?
                .remove("cacheRead");
        }
    }
    if calls {
        value["metadata"] = serde_json::json!({"cacheRequestChange":"tools_changed","cacheObservedCalls":{
            "scope":"a".repeat(64),"attempts":3,"unreported":0,"pairedInput":1100,"pairedRead":1000,
            "knownWrite":0,"unreportedWrites":0,"lostAtLeast":0,"overflow":false,
            "cost":{"status":"subtotal","usd":0.000123,"priced":1,"missingUsage":1,"missingPricing":1,
                "errors":0,"overflow":false,"policy":"pinned-v1","scope":"observed-dispatch-api-equivalent",
                "subscription":"unknown"}
        }});
    }
    Ok(grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
        serde_json::from_value(value)?,
    )))
}
fn client(
    rows: Vec<grapt_api::u3a::SessionMessageInfoDto>,
    partial: bool,
) -> std::result::Result<Client<Peer>, Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            rows,
            partial,
        },
        Bounds::PRODUCTION,
    );
    client.enable_paged_history();
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|m| m.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    client.create_session()?;
    // The stream's positioned connect, then the snapshot it permits.
    for _ in 0..2 {
        client.pump()?;
    }
    Ok(client)
}
pub(crate) fn measured() -> std::result::Result<Snapshot, Box<dyn std::error::Error>> {
    Ok(status::snapshot(&client(
        vec![
            row(1, Some(100), Some(0), true)?,
            row(2, Some(900), Some(900), true)?,
        ],
        false,
    )?))
}
fn backend() -> (Backend, crate::Commands) {
    let (commands, rx) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    (
        Backend {
            commands,
            updates,
            wake,
        },
        rx,
    )
}
fn key(ui: &mut Ui, backend: &Backend, code: KeyCode) -> std::result::Result<(), String> {
    keys::handle_key(KeyEvent::new(code, KeyModifiers::NONE), ui, backend).map(|_| ())
}
fn paint(
    terminal: &mut Terminal<TestBackend>,
    ui: &mut Ui,
) -> std::result::Result<String, Box<dyn std::error::Error>> {
    terminal.draw(|f| ui.draw(f))?;
    let b = terminal.backend().buffer();
    Ok(b.content
        .chunks(usize::from(b.area.width.max(1)))
        .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n"))
}
#[test]
fn current_cache_survives_old_history_browsing_and_distinguishes_capture_from_session() -> Result {
    let mut client = client(
        vec![
            row(1, Some(100), Some(0), true)?,
            row(2, Some(900), Some(900), true)?,
        ],
        false,
    )?;
    let before = status::snapshot(&client);
    assert!(before.compact(80).contains("last 100.0% · session 90.0%"));
    let detail = before.lines(120).join(" ");
    for required in [
        "calls 90.9%",
        "uncached 100",
        "write 0",
        "subtotal API USD 0.000123",
        "1/3 priced",
        "missing usage 1, pricing 1",
        "subscription unknown",
        "3 observed",
        "cause unreported",
        "tools changed",
        "last reported",
    ] {
        assert!(detail.contains(required), "{required}: {detail}");
    }
    client.peer_mut().rows = vec![row(1, Some(100), Some(0), true)?];
    client.browse_history(grapt_client::client::HistoryMove::Beginning)?;
    assert!(client.browsing_history());
    assert_ne!(
        client.display_view().ok_or("reading")?.cache.latest_share(),
        client.view().ok_or("live")?.cache.latest_share()
    );
    assert_eq!(
        status::snapshot(&client),
        before,
        "status must describe selected live session, not the older reading window"
    );
    Ok(())
}
#[test]
fn absent_partial_zero_and_incomplete_usage_do_not_become_free_or_complete() -> Result {
    for (input, read, partial, required) in [
        (None, None, false, "1 message unreported"),
        (Some(100), None, false, "1 message unreported"),
        (Some(0), Some(0), false, "no input"),
        (Some(100), Some(100), true, "history incomplete"),
    ] {
        let client = client(vec![row(1, input, read, false)?], partial)?;
        let snapshot = status::snapshot(&client);
        let text = snapshot.lines(120).join("\n");
        assert!(text.contains(required), "{text}");
        assert!(text.contains("Cost:") && text.contains("not reported"));
        assert!(!text.contains("$0"));
        assert!(snapshot.compact(40).contains("session -"));
    }
    Ok(())
}
#[test]
fn narrow_usage_pane_reaches_all_fields_preserves_draft_and_never_submits() -> Result {
    for (width, height) in [(40, 16), (80, 24), (120, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut ui = Ui::new(false);
        ui.usage = measured()?;
        ui.policy =
            Some("Preset: default | effective: maintainer | applies from the next turn".into());
        ui.composer.insert_str("KEEP 中 draft");
        let (backend, commands) = backend();
        let initial = paint(&mut terminal, &mut ui)?;
        assert!(!initial.contains("Cache last"), "{initial}");
        key(&mut ui, &backend, KeyCode::F(4))?;
        let mut seen = String::new();
        for _ in 0..40 {
            let text = paint(&mut terminal, &mut ui)?;
            assert!(text.contains("Esc/F4 close"));
            seen.push_str(&text);
            // Join visible pane rows across wrapping, excluding border glyphs.
            seen.push_str(
                &text
                    .lines()
                    .filter_map(|line| {
                        let (_, inner) = line.split_once('│')?;
                        Some(inner.split('│').next()?.trim())
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            key(&mut ui, &backend, KeyCode::PageDown)?;
        }
        for required in [
            "Preset: default",
            "effective: maintainer",
            "applies from the next turn",
            "subtotal",
            "API USD",
            "0.000123",
            "1/3 priced",
            "missing usage",
            "pricing 1",
            "subscription",
            "unknown",
            "cause unreported",
            "tools",
            "changed",
            "ses_000000000001",
        ] {
            assert!(
                seen.contains(required),
                "{width}: missing {required}: {seen}"
            );
        }
        for code in [
            KeyCode::Char('x'),
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Home,
            KeyCode::End,
        ] {
            key(&mut ui, &backend, code)?;
        }
        assert!(commands.try_recv().is_err());
        assert_eq!(keys::draft(&ui), "KEEP 中 draft");
        key(&mut ui, &backend, KeyCode::Esc)?;
        assert!(ui.usage_pane.is_none());
        let closed = paint(&mut terminal, &mut ui)?;
        assert!(closed.contains("KEEP") && closed.contains("draft"));
    }
    for (width, height) in [(9, 7), (1, 1), (0, 0)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut ui = Ui::new(false);
        ui.usage = measured()?;
        ui.policy =
            Some("Preset: default | effective: maintainer | applies from the next turn".into());
        ui.usage_pane = Some(Pane::default());
        paint(&mut terminal, &mut ui)?;
    }
    Ok(())
}

#[test]
fn paste_is_ignored_with_usage_open_and_returns_to_normal_after_close() -> Result {
    let mut ui = Ui::new(false);
    let (backend, commands) = backend();
    ui.composer.insert_str("KEPT draft");
    ui.composer
        .input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let cursor = ui.composer.cursor();
    let recall = ui.recall.status();
    key(&mut ui, &backend, KeyCode::F(4))?;
    assert!(!keys::paste("HIDDEN_PASTE", &mut ui));
    assert_eq!(keys::draft(&ui), "KEPT draft");
    assert_eq!(ui.composer.cursor(), cursor);
    assert_eq!(ui.recall.status(), recall);
    assert!(commands.try_recv().is_err());
    key(&mut ui, &backend, KeyCode::F(4))?;
    assert!(keys::paste("OK", &mut ui));
    assert_eq!(keys::draft(&ui), "KEPT drafOKt");
    assert!(commands.try_recv().is_err());
    Ok(())
}
