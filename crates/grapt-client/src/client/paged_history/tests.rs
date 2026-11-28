use super::*;
use crate::{wire::Spelled, ContractDouble};
use grapt_api::u3a::{
    encode_page, BoundedVec, DtoString, SessionMessagesResponseDto,
    SessionMessagesResponseDtoCursor,
};
use grapt_schema::{OptionalNullable, Required};

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    total: usize,
    reads: usize,
    max_rows: usize,
    refuse: bool,
    repeat: bool,
    tools: bool,
}

impl Peer {
    /// The snapshot: the newest window this peer serves.
    fn snapshot(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let session = request
            .path()
            .strip_prefix("/api/session/")
            .and_then(|rest| rest.split('/').next())
            .and_then(|raw| crate::wire::session_id(raw).ok())
            .unwrap_or_else(|| panic!("snapshot path"));
        let window = WireRequest::new(Operation::MessageList, Some(&session), None)
            .unwrap_or_else(|e| panic!("window: {e}"))
            .with_query("limit", "200")
            .with_query("order", "desc");
        let page = self.call(&window)?;
        let body = crate::double::page_snapshot(request, &page.body, 0)
            .unwrap_or_else(|e| panic!("snapshot: {e}"));
        Ok(WireResponse { status: 200, body })
    }
}

impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            return self.snapshot(request);
        }
        if request.operation() != Operation::MessageList {
            return self.inner.call(request);
        }
        self.reads += 1;
        if std::mem::take(&mut self.refuse) {
            return Ok(WireResponse {
                status: 503,
                body: Vec::new(),
            });
        }
        let query = |name: &str| {
            request
                .query()
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(query("limit"), Some("200"));
        assert!(!(query("cursor").is_some() && query("order").is_some()));
        let (descending, offset) = if let Some(cursor) = query("cursor") {
            let (order, offset) = cursor
                .split_once(':')
                .unwrap_or_else(|| panic!("cursor shape"));
            (
                order == "desc",
                offset
                    .parse::<usize>()
                    .unwrap_or_else(|e| panic!("offset: {e}")),
            )
        } else {
            (query("order") == Some("desc"), 0)
        };
        let count = self.total.saturating_sub(offset).min(200);
        self.max_rows = self.max_rows.max(count);
        let order = if descending { "desc" } else { "asc" };
        let next = (offset + count < self.total).then(|| format!("{order}:{}", offset + count));
        let previous = (offset > 0).then(|| format!("{order}:{}", offset.saturating_sub(200)));
        let mut rows = Vec::new();
        for index in offset..offset + count {
            let number = if descending {
                self.total - 1 - index
            } else {
                index
            };
            rows.push(if self.tools {
                crate::wire::assistant_message(
                    &format!("msg_{number:012x}"),
                    &[crate::wire::MessageItem::Tool(crate::wire::DurableTool {
                        duration_ms: None,
                        id: crate::wire::tool_call_id("call_window")
                            .unwrap_or_else(|e| panic!("call: {e}")),
                        name: "shell".into(),
                        progress: vec![],
                        state: crate::wire::DurableToolState::Completed {
                            input: "{}".into(),
                            output: format!("tool {number:05}"),
                        },
                    })],
                    None,
                    None,
                    None,
                )
                .unwrap_or_else(|e| panic!("tool DTO: {e}"))
            } else {
                crate::wire::user_message(
                    &format!("msg_{number:012x}"),
                    &format!("marker {number:05}"),
                )
                .unwrap_or_else(|e| panic!("DTO row: {e}"))
            });
        }
        let next = if std::mem::take(&mut self.repeat) {
            Some(query("cursor").unwrap_or("repeat").to_owned())
        } else {
            next
        };
        let cursor_value = |value: Option<String>| {
            value.map(|text| DtoString::try_new(text).unwrap_or_else(|e| panic!("cursor: {e}")))
        };
        // Keep the codec's discriminant-first field order for message unions.
        let page = SessionMessagesResponseDto::new(
            Required::new(BoundedVec::try_new(rows).unwrap_or_else(|e| panic!("rows: {e}"))),
            Required::new(SessionMessagesResponseDtoCursor::new(
                cursor_value(previous)
                    .map_or_else(OptionalNullable::absent, OptionalNullable::value),
                cursor_value(next).map_or_else(OptionalNullable::absent, OptionalNullable::value),
            )),
        );
        let body = encode_page(
            &page,
            crate::wire::limits().unwrap_or_else(|e| panic!("limits: {e}")),
        )
        .unwrap_or_else(|e| panic!("DTO page: {e}"));
        assert!(body.len() <= Bounds::PRODUCTION.max_response_bytes);
        Ok(WireResponse { status: 200, body })
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}

fn client_with(total: usize) -> Result<(Client<Peer>, SessionWireId), ClientError> {
    let mut inner = ContractDouble::new();
    let session = inner
        .create_session("long transcript")
        .unwrap_or_else(|| panic!("session"));
    let mut client = Client::new(
        Peer {
            inner,
            total,
            reads: 0,
            max_rows: 0,
            refuse: false,
            repeat: false,
            tools: false,
        },
        Bounds::PRODUCTION,
    );
    client.enable_paged_history();
    client.select(&session)?;
    crate::client::tests::prove(&mut client);
    Ok((client, session))
}

#[test]
fn reading_a_family_preview_keeps_selection_controls_and_older_page_owned_by_the_client(
) -> Result<(), ClientError> {
    let (mut client, selected) = client_with(10_000)?;
    client.browse_history(HistoryMove::Older)?;
    let reader = client.display_view().cloned();
    let edges = client.history_edges();
    let permissions = client.permissions().to_vec();
    let other = client
        .peer_mut()
        .inner
        .create_session("child preview")
        .unwrap_or_else(|| panic!("child session"));
    let tail = client.recent_session_view(&other)?;
    assert!(tail.slices.len() <= 32);
    assert!(tail
        .slices
        .last()
        .is_some_and(|row| row.text.contains("09999")));
    assert_eq!(client.selected(), Some(&selected));
    assert_eq!(client.display_view().cloned(), reader);
    assert_eq!(client.history_edges(), edges);
    assert_eq!(client.permissions(), permissions);
    Ok(())
}

#[test]
fn an_oversized_window_page_uses_the_common_ingress_bound_and_preserves_the_reader(
) -> Result<(), ClientError> {
    let (mut client, _) = client_with(10_000)?;
    client.browse_history(HistoryMove::Older)?;
    let before = client.display_view().cloned();
    client.bounds.max_response_bytes = 1;
    assert!(matches!(
        client.browse_history(HistoryMove::Older),
        Err(ClientError::Transport(TransportError::ResponseTooLarge {
            bound: 1
        }))
    ));
    assert_eq!(client.display_view().cloned(), before);
    Ok(())
}

#[test]
fn tool_hints_refresh_one_newest_page_in_chronological_order_without_moving_the_reader(
) -> Result<(), ClientError> {
    let (mut client, _) = client_with(10_000)?;
    client.browse_history(HistoryMove::Older)?;
    let reader = client.display_view().cloned();
    client.peer_mut().tools = true;
    let before = client.peer_mut().reads;
    let calls = [crate::wire::tool_call_id("call_window")?]
        .into_iter()
        .collect();
    assert!(client.refresh_tool_rows(calls)?);
    assert_eq!(client.peer_mut().reads, before + 1);
    assert_eq!(client.display_view().cloned(), reader);
    assert!(client.view().is_some_and(|view| view
        .slices
        .iter()
        .any(|row| row.text.contains("tool 09999"))));
    assert!(!client.view().is_some_and(|view| view
        .slices
        .iter()
        .any(|row| row.text.contains("tool 09800"))));
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn newest_page_is_input_ready_after_one_read_and_every_message_is_reachable(
) -> Result<(), ClientError> {
    let (mut client, session) = client_with(50_000)?;
    assert_eq!(client.peer_mut().reads, 1);
    assert!(client.stream_state().is_live());
    assert_eq!(
        client.older_history(),
        &super::super::OlderHistory::Available
    );
    assert!(
        client.recovery.backfill.is_none(),
        "manual navigation owns older reads"
    );
    client.pump_at(std::time::Duration::ZERO)?;
    assert_eq!(
        client.peer_mut().reads,
        1,
        "a quiet manual window does not backfill"
    );
    assert_eq!(
        client
            .display_view()
            .and_then(|v| v.slices.last())
            .map(|s| s.text.as_str()),
        Some("marker 49999")
    );
    let tail = client.view().cloned();
    let tail_edges = client.history_edges();
    assert!(client.browse_history(HistoryMove::Older)?);
    let reads = client.peer_mut().reads;
    assert!(client.browse_history(HistoryMove::Cancel)?);
    assert_eq!(
        client.peer_mut().reads,
        reads,
        "cancelling never fetches history"
    );
    assert_eq!(client.display_view().cloned(), tail);
    assert_eq!(client.history_edges(), tail_edges);
    assert!(!client.browse_history(HistoryMove::Cancel)?);
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let view = client
            .display_view()
            .unwrap_or_else(|| panic!("reading view"));
        assert!(view.slices.len() <= 200);
        for row in &view.slices {
            assert!(seen.insert(row.text.clone()), "no repeated page or marker");
        }
        if !client.browse_history(HistoryMove::Older)? {
            break;
        }
    }
    assert_eq!(seen.len(), 50_000);
    assert!(seen.contains("marker 00000") && seen.contains("marker 49999"));
    assert_eq!(
        client.view().cloned(),
        tail,
        "browsing does not replace the live projection"
    );
    assert_eq!(client.selected(), Some(&session));
    assert!(client.browse_history(HistoryMove::Newer)?);
    client.browse_history(HistoryMove::Latest)?;
    assert_eq!(client.display_view(), client.view());
    let reads = client.peer_mut().reads;
    assert!(client.browse_history(HistoryMove::Beginning)?);
    assert_eq!(
        client.peer_mut().reads,
        reads + 1,
        "jump to beginning does not walk the transcript"
    );
    assert_eq!(
        client
            .display_view()
            .and_then(|v| v.slices.first())
            .map(|s| s.text.as_str()),
        Some("marker 00000")
    );
    assert!(client.browse_history(HistoryMove::Newer)?);
    assert_eq!(
        client
            .display_view()
            .and_then(|v| v.slices.first())
            .map(|s| s.text.as_str()),
        Some("marker 00200")
    );
    assert!(client.peer_mut().max_rows <= 200);
    assert_eq!(
        session.spelled(),
        client
            .display_view()
            .map(|v| v.session_id.spelled())
            .unwrap_or_default()
    );
    Ok(())
}

#[test]
fn failed_or_repeated_page_preserves_the_reader_and_new_tail_recovery_does_not_move_it(
) -> Result<(), ClientError> {
    let (mut client, _) = client_with(10_000)?;
    assert!(client.browse_history(HistoryMove::Older)?);
    let before = client.display_view().cloned();
    for repeat in [false, true] {
        client.peer_mut().repeat = repeat;
        client.peer_mut().refuse = !repeat;
        assert!(client.browse_history(HistoryMove::Older).is_err());
        assert_eq!(client.display_view().cloned(), before);
    }
    // The stream is cut; the reconnect proves itself before its snapshot.
    client
        .peer_mut()
        .inner
        .set_fault(crate::double::Fault::CloseStream);
    let reads = client.peer_mut().reads;
    for _ in 0..3 {
        client.pump_at(std::time::Duration::ZERO)?;
    }
    assert_eq!(
        client.peer_mut().reads,
        reads + 1,
        "recovery fetches the newest bounded page only"
    );
    assert_eq!(client.display_view().cloned(), before);
    assert!(client.stream_state().is_live());
    assert_eq!(client.recovery_wait_at(std::time::Duration::ZERO), None);
    Ok(())
}
