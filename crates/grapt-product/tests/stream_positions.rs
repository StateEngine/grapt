//! Two subscriber classes share the real authenticated route and producer.
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use grapt_api::u3a::{
    AggregateIdDto, DogfoodEvent, DtoString, DurableEvent, DurableEventRef, FiniteF64, LiveEvent,
    ServerConnectedEvent, SliceDeltaData, SliceEndedData, SliceStartedData,
};
use grapt_api::{
    ApiHostLimits, ApiRouterBuilder, AuthenticatedApiService, AuthenticationPolicyVersion,
    DogfoodEventHub, DogfoodEventSubscription, HeaderReadAllowance, MaxConnections, MaxHeaders,
    MaxInflightRequests, MaxInflightStreams, MaxPrincipalMappings, MaxRequestBodyBytes,
    MaxRequestHeadBytes, MaxRequestsPerConnection, MaxRouterModules, PreparedApiHost,
    ProvisionalBlock, ProvisionalChange, ProvisionalKind, RunningApiHost, SeatDirectoryEntry,
    SeatPrincipalDirectory, SeatTrustBoundary, ShutdownDrainAllowance, SseLimits,
    StreamWriteAllowance, TokioDeadlineDriver, WireId,
};
use grapt_client::{Bounds, EventEnvelope, SessionView, SseReader};
use grapt_location::{
    BindHost, BindLimits, BindPort, ListenerBindPlan, ListenerBinder, LiteralAddressResolver,
    MaxBindAttempts, MaxResolvedAddresses,
};
use grapt_types::{ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, Timestamp};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SESSION: &str = "ses_position_consumers";
const MESSAGE: &str = "msg_position_consumers";

#[derive(Debug)]
struct Authority;
impl ClockAuthority for Authority {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1)
    }
}
impl IdAuthority for Authority {
    fn mint(&self) -> IdToken {
        IdToken::new("positions-fixture").unwrap_or_else(|| panic!("fixture token"))
    }
}

fn wire<I: grapt_api::u3a::codec::WireIdentity>(raw: &str) -> WireId<I> {
    WireId::parse(raw.to_owned()).unwrap_or_else(|| panic!("fixture id: {raw}"))
}

fn hub() -> TestResult<Arc<DogfoodEventHub>> {
    Ok(Arc::new(DogfoodEventHub::try_new(
        SseLimits::production()?,
        ServerConnectedEvent::new(wire("evt_position_connected")),
    )?))
}

async fn host(hub: Arc<DogfoodEventHub>) -> TestResult<RunningApiHost> {
    let directory = SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            grapt_api::SubjectId::try_new("alpha")?,
            grapt_types::SeatRecipientId::try_new("seat-a")?,
        )],
        MaxPrincipalMappings::try_new(1)?,
        &DeterministicAuthorities::new(&Authority, &Authority),
    )?;
    let boundary = SeatTrustBoundary::with_injected_basic_verifier(
        AuthenticationPolicyVersion::try_new("positions-fixture")?,
        directory,
        grapt_auth_basic::BasicAuthVerifier::new("alpha", "zephyr")
            .map_err(|error| format!("fixture credential: {error:?}"))?,
    );
    let limits = ApiHostLimits {
        max_connections: MaxConnections::try_new(8)?,
        max_requests_per_connection: MaxRequestsPerConnection::try_new(4)?,
        max_inflight_requests: MaxInflightRequests::try_new(4)?,
        max_inflight_streams: MaxInflightStreams::try_new(4)?,
        max_request_head_bytes: MaxRequestHeadBytes::try_new(8192)?,
        max_headers: MaxHeaders::try_new(16)?,
        max_request_body_bytes: MaxRequestBodyBytes::try_new(1024)?,
        header_read_allowance: HeaderReadAllowance::try_new(Duration::from_secs(5))?,
        shutdown_drain_allowance: ShutdownDrainAllowance::try_new(Duration::from_secs(5))?,
        stream_write_allowance: StreamWriteAllowance::try_new(Duration::from_secs(30))?,
    };
    let mut routes = ApiRouterBuilder::new(MaxRouterModules::try_new(4)?);
    grapt_api::register_dogfood_event_route(&mut routes, hub)?;
    let service = AuthenticatedApiService::new(
        routes.seal(),
        Arc::new(boundary),
        limits.max_request_body_bytes,
    );
    let listener = ListenerBinder::new(Arc::new(LiteralAddressResolver))
        .bind(&ListenerBindPlan::new(
            BindHost::Ip("127.0.0.1".parse()?),
            BindPort::Ephemeral,
            BindLimits::new(MaxResolvedAddresses::new(1)?, MaxBindAttempts::new(1)?),
        ))
        .await?;
    Ok(PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )?
    .start())
}

fn subscribe(address: SocketAddr, query: &str) -> TestResult<BufReader<TcpStream>> {
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    // HTTP/1.0 is close-delimited, so the SSE reader receives body bytes directly.
    write!(socket, "GET /api/event{query} HTTP/1.0\r\nHost: fixture\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\n\r\n")?;
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    assert!(line.contains(" 200 "), "subscription: {line}");
    loop {
        line.clear();
        assert!(reader.read_line(&mut line)? > 0, "incomplete response head");
        if line == "\r\n" {
            break;
        }
        assert!(!line.to_ascii_lowercase().starts_with("transfer-encoding:"));
    }
    Ok(reader)
}

fn next(reader: &mut BufReader<TcpStream>) -> TestResult<Option<String>> {
    let mut frame = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            assert!(frame.is_empty(), "stream ended inside an SSE frame");
            return Ok(None);
        }
        assert!(
            frame.len().saturating_add(line.len()) <= grapt_api::u3a::MAX_POSITIONED_FRAME_BYTES
        );
        let end = line == "\n" || line == "\r\n";
        frame.push_str(&line);
        if end {
            return Ok(Some(frame));
        }
    }
}

fn data(frame: &str) -> TestResult<serde_json::Value> {
    Ok(serde_json::from_str(
        frame
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .ok_or("data field")?,
    )?)
}

struct Positioned {
    reader: BufReader<TcpStream>,
    prior: Option<(String, u64)>,
}
impl Positioned {
    fn next(&mut self) -> TestResult<(String, serde_json::Value)> {
        let raw = next(&mut self.reader)?.ok_or("positioned frame absent")?;
        let id = raw
            .lines()
            .find_map(|line| line.strip_prefix("id: "))
            .ok_or("position absent")?
            .to_owned();
        let (epoch, seq) = id.rsplit_once('-').ok_or("position format")?;
        let seq = seq.parse::<u64>()?;
        if let Some((prior_epoch, prior_seq)) = &self.prior {
            assert_eq!(epoch, prior_epoch);
            assert!(seq > *prior_seq);
        }
        self.prior = Some((epoch.to_owned(), seq));
        Ok((id, data(&raw)?))
    }
}

fn legacy(
    reader: &mut BufReader<TcpStream>,
    control: &DogfoodEventSubscription,
    codec: &mut SseReader,
    view: &mut SessionView,
) -> TestResult<serde_json::Value> {
    let raw = next(reader)?.ok_or("legacy frame absent")?;
    let expected = control
        .next_frame()?
        .ok_or("legacy-only control frame absent")?;
    assert_eq!(
        raw.as_bytes(),
        expected.as_ref(),
        "mixed registration changed legacy bytes"
    );
    assert!(!raw.lines().any(|line| line.starts_with("id:")));
    codec.push_bytes(raw.as_bytes())?;
    let payload = codec.take().ok_or("legacy decoder lost frame")?;
    let event = EventEnvelope::decode(&payload)?;
    view.apply(&event, Bounds::PRODUCTION)?;
    let value = data(&raw)?;
    assert!(value["data"].get("from").is_none());
    Ok(value)
}

fn started(kind: ProvisionalKind, ordinal: u64) -> DogfoodEvent {
    let event = DurableEvent::new(
        wire(&format!("evt_position_start_{ordinal}")),
        FiniteF64::try_new(1.0).unwrap_or_else(|e| panic!("time: {e}")),
        DurableEventRef::new(
            AggregateIdDto::try_new(SESSION).unwrap_or_else(|e| panic!("aggregate: {e}")),
            (ordinal * 10 + 1).into(),
        ),
        SliceStartedData::new(wire(SESSION), wire(MESSAGE), ordinal.into()),
    );
    match kind {
        ProvisionalKind::Text => DogfoodEvent::TextStarted(event),
        ProvisionalKind::Reasoning => DogfoodEvent::ReasoningStarted(event),
    }
}

fn delta(kind: ProvisionalKind, ordinal: u64, text: &str) -> TestResult<DogfoodEvent> {
    let event = LiveEvent::new(
        wire(&format!("evt_position_delta_{ordinal}_{}", text.len())),
        FiniteF64::try_new(1.0)?,
        SliceDeltaData::new(
            wire(SESSION),
            wire(MESSAGE),
            ordinal.into(),
            DtoString::try_new(text)?,
        ),
    );
    Ok(match kind {
        ProvisionalKind::Text => DogfoodEvent::TextDelta(event),
        ProvisionalKind::Reasoning => DogfoodEvent::ReasoningDelta(event),
    })
}

fn publish_blocks(
    hub: &DogfoodEventHub,
    control_hub: &DogfoodEventHub,
    old: &mut BufReader<TcpStream>,
    control: &DogfoodEventSubscription,
    codec: &mut SseReader,
    view: &mut SessionView,
    positioned: &mut Positioned,
) -> TestResult {
    let session = SESSION.parse()?;
    for (ordinal, kind) in [(0, ProvisionalKind::Text), (1, ProvisionalKind::Reasoning)] {
        let block = ProvisionalBlock {
            message: MESSAGE.parse()?,
            kind,
            ordinal,
        };
        let event = started(kind, ordinal);
        for target in [hub, control_hub] {
            target.publish_provisional(
                &event,
                ProvisionalChange::Open {
                    session: &session,
                    block: &block,
                },
            )?;
        }
        assert_eq!(positioned.next()?.1, legacy(old, control, codec, view)?);
        let mut offset = 0u64;
        for text in ["é🙂", "x"] {
            let event = delta(kind, ordinal, text)?;
            for target in [hub, control_hub] {
                target.publish_provisional(
                    &event,
                    ProvisionalChange::Append {
                        session: &session,
                        block: &block,
                        text,
                    },
                )?;
            }
            let mut current = positioned.next()?.1;
            assert_eq!(current["data"]["from"].as_u64(), Some(offset));
            current["data"]
                .as_object_mut()
                .ok_or("delta object")?
                .remove("from");
            assert_eq!(current, legacy(old, control, codec, view)?);
            offset += u64::try_from(text.len())?;
        }
        let event = DurableEvent::new(
            wire(&format!("evt_position_end_{ordinal}")),
            FiniteF64::try_new(1.0)?,
            DurableEventRef::new(
                AggregateIdDto::try_new(SESSION)?,
                ((ordinal + 1) * 10).into(),
            ),
            SliceEndedData::new(
                wire(SESSION),
                wire(MESSAGE),
                ordinal.into(),
                DtoString::try_new("é🙂x")?,
            ),
        );
        let event = match kind {
            ProvisionalKind::Text => DogfoodEvent::TextEnded(event),
            ProvisionalKind::Reasoning => DogfoodEvent::ReasoningEnded(event),
        };
        for target in [hub, control_hub] {
            target.publish_provisional(
                &event,
                ProvisionalChange::Close {
                    session: &session,
                    block: &block,
                },
            )?;
        }
        assert_eq!(positioned.next()?.1, legacy(old, control, codec, view)?);
    }
    Ok(())
}

#[test]
fn mixed_subscribers_keep_legacy_decoding_and_positioned_utf8_offsets() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let hub = hub()?;
    let control_hub = self::hub()?;
    let control = control_hub.subscribe_host()?;
    let host = runtime.block_on(host(Arc::clone(&hub)))?;
    let address = host.ready().observed_address();
    let mut old = subscribe(address, "")?;
    let mut positioned = Positioned {
        reader: subscribe(address, "?positions=1")?,
        prior: None,
    };
    let mut codec = SseReader::new(Bounds::PRODUCTION);
    let mut view = SessionView::new(&grapt_client::wire::session_id(SESSION)?);
    let initial = legacy(&mut old, &control, &mut codec, &mut view)?;
    assert_eq!(positioned.next()?.1, initial);
    publish_blocks(
        &hub,
        &control_hub,
        &mut old,
        &control,
        &mut codec,
        &mut view,
        &mut positioned,
    )?;
    assert_eq!(view.slices.len(), 2);
    assert!(view
        .slices
        .iter()
        .all(|slice| slice.text == "é🙂x" && slice.closed));
    hub.reconcile();
    control_hub.reconcile();
    let (_, resync) = positioned.next()?;
    assert_eq!(resync["type"], "stream.resync");
    assert_eq!(
        resync["data"]["epoch"].as_str(),
        positioned.prior.as_ref().map(|(epoch, _)| epoch.as_str())
    );
    assert!(resync["data"].get("sessionID").is_none());
    assert!(next(&mut old)?.is_none());
    assert!(next(&mut positioned.reader)?.is_none());
    drop(old);
    drop(positioned);
    runtime.block_on(host.stop());
    Ok(())
}
