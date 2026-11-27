use super::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::future::poll_fn;

use serde::Serialize;

use crate::auth::{AuthenticationPolicyVersion, SeatTrustBoundary, SubjectId};
use crate::directory::{MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory};
use crate::u3a::dto::NonNegativeU64;
use crate::u3a::{
    AggregateIdDto, ApplicationFuture, DurableEvent, DurableEventRef, FiniteF64, PublishReceipt,
    ServerConnectedEvent, SliceStartedData, SseLimitValues, WireId, MAX_SSE_EVENT_BYTES,
    MAX_SSE_FRAME_BYTES,
};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, LocationRef, SeatRecipientId,
    Timestamp,
};

// -----------------------------------------------------------------------
// Fixture authorities and principal: the same deterministic chain as the
// crate-level witnesses. No ambient clock, no ambient randomness, so a
// replay of the fixture is byte-identical.
// -----------------------------------------------------------------------

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug)]
struct CountingIds {
    next: Cell<u64>,
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.next.get();
        self.next.set(ordinal.saturating_add(1));
        match IdToken::new(format!("cli-{ordinal}")) {
            Some(token) => token,
            None => panic!("the counting id authority produced an empty token"),
        }
    }
}

fn fixture_principal(subject_raw: &str, recipient_raw: &str) -> SealedPrincipal {
    let subject = match SubjectId::try_new(subject_raw) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture subject {subject_raw:?} was refused: {refusal}"),
    };
    let recipient = match SeatRecipientId::try_new(recipient_raw) {
        Ok(value) => value,
        Err(error) => panic!("fixture recipient {recipient_raw:?} was refused: {error:?}"),
    };
    let policy = match AuthenticationPolicyVersion::try_new("auth-policy-v1") {
        Ok(value) => value,
        Err(refusal) => panic!("fixture policy was refused: {refusal}"),
    };
    let bound = match MaxPrincipalMappings::try_new(8) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture bound was refused: {refusal}"),
    };
    let clock = FixedClock(1_700_000_000_000);
    let ids = CountingIds { next: Cell::new(0) };
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let directory = match SeatPrincipalDirectory::configure(
        vec![SeatDirectoryEntry::new(
            match SubjectId::try_new(subject_raw) {
                Ok(value) => value,
                Err(refusal) => panic!("fixture subject {subject_raw:?} was refused: {refusal}"),
            },
            recipient,
        )],
        bound,
        &authorities,
    ) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture directory was refused: {refusal}"),
    };
    let boundary = SeatTrustBoundary::new(policy, directory);
    let authenticated = match boundary.authenticate_fixture(subject) {
        Ok(value) => value,
        Err(refusal) => panic!("fixture authentication was refused: {refusal}"),
    };
    SealedPrincipal::seal(authenticated)
}

// -----------------------------------------------------------------------
// Row, coordinate and event fixtures. Every identity is typed; every
// refusal panics with a diagnostic instead of inventing a value.
// -----------------------------------------------------------------------

fn dto_string_fixture(raw: &str) -> DtoString {
    match DtoString::try_new(raw) {
        Ok(value) => value,
        Err(error) => panic!("fixture string {raw:?} was refused: {error}"),
    }
}

fn session_id_fixture(raw: &str) -> SessionId {
    match raw.parse::<SessionId>() {
        Ok(value) => value,
        Err(error) => panic!("fixture session {raw:?} was refused: {error:?}"),
    }
}

fn coordinate_fixture(raw: &str, seq: u64) -> CaptureCoordinate {
    CaptureCoordinate {
        aggregate: session_id_fixture(raw),
        seq: EventSeq::new(seq),
        fencing: EventFencing::LegacyUnfenced,
    }
}

fn slice_started(session: &str, seq: u64) -> DogfoodEvent {
    let id = match WireId::parse(format!("evt_{session}_{seq}")) {
        Some(value) => value,
        None => panic!("fixture event id was refused"),
    };
    let created = match FiniteF64::try_new(1.5) {
        Ok(value) => value,
        Err(error) => panic!("fixture timestamp was refused: {error}"),
    };
    let aggregate = match AggregateIdDto::try_new(session) {
        Ok(value) => value,
        Err(error) => panic!("fixture aggregate was refused: {error}"),
    };
    let session_id = match WireId::parse(format!("ses_{session}")) {
        Some(value) => value,
        None => panic!("fixture session wire id was refused"),
    };
    let message = match WireId::parse(format!("msg_{session}_1")) {
        Some(value) => value,
        None => panic!("fixture message wire id was refused"),
    };
    DogfoodEvent::TextStarted(DurableEvent::new(
        id,
        created,
        DurableEventRef::new(aggregate, NonNegativeU64::from(seq)),
        SliceStartedData::new(session_id, message, NonNegativeU64::from(0)),
    ))
}

fn event_row(session: &str, seq: u64) -> CapturePageRow {
    CapturePageRow::Event {
        coordinate: coordinate_fixture(session, seq),
        event: Box::new(slice_started(session, seq)),
    }
}

fn skipped_row(session: &str, seq: u64) -> CapturePageRow {
    CapturePageRow::Skipped {
        coordinate: coordinate_fixture(session, seq),
        kind: dto_string_fixture("session.reasoning.started"),
    }
}

fn page_fixture(rows: Vec<CapturePageRow>, continuation: Option<CaptureCursor>) -> CapturePage {
    CapturePage { rows, continuation }
}

fn decode_fixture(token: &str, interest: &CaptureInterest) -> CaptureCursor {
    match CaptureCursor::decode(token, interest) {
        Ok(value) => value,
        Err(error) => panic!("the minted token would not decode: {error:?}"),
    }
}

fn limits_fixture(pending_events: usize, frame_bytes: usize) -> SseLimits {
    match SseLimits::try_new(SseLimitValues {
        subscribers: 16,
        pending_events,
        event_bytes: MAX_SSE_EVENT_BYTES,
        frame_bytes,
    }) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits fixture failed: {error}"),
    }
}

fn production_limits_fixture() -> SseLimits {
    match SseLimits::production() {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits fixture failed: {error}"),
    }
}

fn encode_sse_fixture<T: Serialize>(value: &T, limits: SseLimits) -> Bytes {
    match encode_sse(value, limits) {
        Ok(bytes) => bytes,
        Err(error) => panic!("the fixture frame would not encode: {error}"),
    }
}

fn hub_fixture(limits: SseLimits) -> Arc<DogfoodEventHub> {
    let id = match WireId::parse("evt_hub".to_owned()) {
        Some(value) => value,
        None => panic!("connected fixture id was refused"),
    };
    let hub = match DogfoodEventHub::try_new(limits, ServerConnectedEvent::new(id)) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub fixture failed: {error}"),
    };
    Arc::new(hub)
}

fn publish_receipt(hub: &DogfoodEventHub, event: &DogfoodEvent) -> PublishReceipt {
    match hub.publish(event) {
        Ok(receipt) => receipt,
        Err(error) => panic!("the publish failed: {error}"),
    }
}

fn publish_at_receipt(
    hub: &DogfoodEventHub,
    event: &DogfoodEvent,
    location: &LocationRef,
) -> PublishReceipt {
    match hub.publish_at(event, location) {
        Ok(receipt) => receipt,
        Err(error) => panic!("the scoped publish failed: {error}"),
    }
}

fn location_fixture(raw: &str) -> LocationRef {
    match LocationRef::new(raw, None) {
        Ok(value) => value,
        Err(error) => panic!("fixture location {raw:?} was refused: {error:?}"),
    }
}

fn interest_fixture(raw: &str) -> CaptureInterest {
    match CaptureInterest::location(raw.to_owned(), None) {
        Ok(value) => value,
        Err(error) => panic!("fixture interest {raw:?} was refused: {error:?}"),
    }
}

fn pct(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// The token codec's own base64url, mirrored here so a hand-built fixture
// token is canonical without depending on the private encoder.
fn base64url_encode(payload: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(payload.len().div_ceil(3) * 4);
    let mut index = 0usize;
    while index + 3 <= payload.len() {
        let triple = (payload[index] as u32) << 16
            | (payload[index + 1] as u32) << 8
            | payload[index + 2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        index += 3;
    }
    match payload.len() - index {
        1 => {
            let pair = (payload[index] as u32) << 16;
            out.push(ALPHABET[(pair >> 18) as usize] as char);
            out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let pair = (payload[index] as u32) << 16 | (payload[index + 1] as u32) << 8;
            out.push(ALPHABET[(pair >> 18) as usize] as char);
            out.push(ALPHABET[((pair >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((pair >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    out
}

// -----------------------------------------------------------------------
// The application double: a scripted page source that records every call
// and can commit live frames in the exact instant a page is served.
// -----------------------------------------------------------------------

struct ScriptedPage {
    page: CapturePage,
    committed_just_before: Vec<(DogfoodEvent, Option<LocationRef>)>,
}

enum PortOutcome {
    Page(ScriptedPage),
    Refusal(CapturePageError),
}

#[derive(Default)]
struct PortDouble {
    script: Mutex<VecDeque<PortOutcome>>,
    calls: Mutex<Vec<(CaptureInterest, CaptureCursor)>>,
    widths: Mutex<VecDeque<Result<u64, CapturePageError>>>,
    hub: Mutex<Option<Arc<DogfoodEventHub>>>,
}

impl PortDouble {
    fn new() -> Self {
        Self::default()
    }

    fn with_hub(hub: Arc<DogfoodEventHub>) -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            widths: Mutex::new(VecDeque::new()),
            hub: Mutex::new(Some(hub)),
        }
    }

    fn push_page(&self, page: CapturePage) {
        self.push_page_committing(page, vec![]);
    }

    fn push_page_committing(
        &self,
        page: CapturePage,
        committed: Vec<(DogfoodEvent, Option<LocationRef>)>,
    ) {
        let mut script = self
            .script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        script.push_back(PortOutcome::Page(ScriptedPage {
            page,
            committed_just_before: committed,
        }));
    }

    fn push_refusal(&self, refusal: CapturePageError) {
        let mut script = self
            .script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        script.push_back(PortOutcome::Refusal(refusal));
    }

    fn push_width(&self, width: u64) {
        let mut widths = self
            .widths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        widths.push_back(Ok(width));
    }

    fn push_width_refusal(&self, refusal: CapturePageError) {
        let mut widths = self
            .widths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        widths.push_back(Err(refusal));
    }

    fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

impl CaptureApplicationPort for PortDouble {
    fn capture_page(
        &self,
        _principal: SealedPrincipal,
        interest: CaptureInterest,
        cursor: CaptureCursor,
    ) -> ApplicationFuture<CapturePage, CapturePageError> {
        {
            let mut calls = self
                .calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            calls.push((interest.clone(), cursor.clone()));
        }
        let hub = self
            .hub
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let outcome = match self
            .script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
        {
            Some(outcome) => outcome,
            None => {
                return Box::pin(async {
                    Err(CapturePageError::UnreadableStore {
                        detail: dto_string_fixture("the port script is exhausted"),
                    })
                });
            }
        };
        match outcome {
            PortOutcome::Page(scripted) => {
                if let Some(hub) = hub {
                    for (event, location) in &scripted.committed_just_before {
                        let receipt = match location {
                            Some(location) => hub.publish_at(event, location),
                            None => hub.publish(event),
                        };
                        match receipt {
                            Ok(receipt) if receipt.delivered > 0 => {}
                            Ok(receipt) => panic!(
                                "a pre-committed frame reached no subscriber (delivered {})",
                                receipt.delivered
                            ),
                            Err(error) => panic!("a pre-committed publish failed: {error}"),
                        }
                    }
                }
                let page = scripted.page;
                Box::pin(async move { Ok(page) })
            }
            PortOutcome::Refusal(refusal) => Box::pin(async move { Err(refusal) }),
        }
    }

    fn count_capture_aggregates(
        &self,
        _principal: SealedPrincipal,
        _interest: CaptureInterest,
    ) -> ApplicationFuture<u64, CapturePageError> {
        // The width probe is synchronous and is not recorded as a page
        // call: the existing call_count assertions keep counting pages.
        // An unscripted probe answers zero, the narrowest honest width.
        let width = self
            .widths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or(Ok(0));
        Box::pin(async move { width })
    }
}

// -----------------------------------------------------------------------
// The transport driver: the handler is invoked directly, and the
// resulting body is polled frame by frame.
// -----------------------------------------------------------------------

async fn open_capture(
    uri: &str,
    principal: Option<SealedPrincipal>,
    hub: &Arc<DogfoodEventHub>,
    port: &Arc<PortDouble>,
) -> Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(principal) = principal {
        builder = builder.extension(principal);
    }
    let request = match builder.body(Body::empty()) {
        Ok(request) => request,
        Err(error) => panic!("fixture request was refused: {error}"),
    };
    let port_arc = Arc::clone(port);
    let application: Arc<dyn CaptureApplicationPort> = port_arc;
    capture_event_handler(request, Arc::clone(hub), application).await
}

fn response_parts(response: Response) -> (StatusCode, Option<String>, Body) {
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (status, content_type, response.into_body())
}

async fn next_frame<B>(body: &mut B) -> Option<Result<Frame<Bytes>, B::Error>>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await
}

async fn next_frame_ok<B>(body: &mut B) -> Frame<Bytes>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    match next_frame(body).await {
        Some(Ok(frame)) => frame,
        other => panic!("the stream did not hand a data frame: {other:?}"),
    }
}

fn frame_text(frame: Frame<Bytes>, what: &str) -> String {
    let bytes = match frame.into_data() {
        Ok(bytes) => bytes,
        Err(_) => panic!("{what}: the frame was not data"),
    };
    match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_owned(),
        Err(error) => panic!("{what}: the frame was not UTF-8: {error}"),
    }
}

fn frame_data(text: &str, what: &str) -> serde_json::Value {
    let data = match text.strip_prefix("event: message\ndata: ") {
        Some(rest) => match rest.strip_suffix("\n\n") {
            Some(data) => data,
            None => panic!("{what}: the frame has no trailing blank line"),
        },
        None => panic!("{what}: the frame is not an SSE message"),
    };
    match serde_json::from_str(data) {
        Ok(value) => value,
        Err(error) => panic!("{what}: the frame JSON was refused: {error}"),
    }
}

fn terminated_fields(text: &str, what: &str) -> (String, String) {
    let value = frame_data(text, what);
    let reason = match value.get("reason").and_then(|entry| entry.as_str()) {
        Some(reason) => reason.to_owned(),
        None => panic!("{what}: the terminal frame carries no reason"),
    };
    let cursor = match value.get("cursor").and_then(|entry| entry.as_str()) {
        Some(cursor) => cursor.to_owned(),
        None => panic!("{what}: the terminal frame carries no cursor"),
    };
    (reason, cursor)
}

async fn assert_pending<B>(body: &mut B, what: &str)
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    poll_fn(|context| {
        if Pin::new(&mut *body).poll_frame(context).is_ready() {
            panic!("{what}: the stream was ready when it should have been pending");
        }
        Poll::Ready(())
    })
    .await;
}

async fn assert_eof<B>(body: &mut B, what: &str)
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    match next_frame(body).await {
        None => {}
        Some(frame) => panic!("{what}: the stream ended with a frame instead of EOF: {frame:?}"),
    }
}

// -----------------------------------------------------------------------
// J03 pre-stream refusals; the eleven stream journeys moved to
// capture_journey_tests.rs (declared below). Names are the EVR witness inventory.
// -----------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_capture_is_refused_before_any_port_call() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::new());
    let response = open_capture(CAPTURE_EVENT_PATH, None, &hub, &port).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(port.call_count(), 0);
}

#[tokio::test]
async fn an_invalid_capture_query_refuses_before_any_stream() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::new());
    let principal = Some(fixture_principal("delta", "seat-delta"));
    let uri = format!("{CAPTURE_EVENT_PATH}?location=oops");
    let response = open_capture(&uri, principal, &hub, &port).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(port.call_count(), 0);
}

#[tokio::test]
async fn malformed_and_wrong_version_cursors_refuse_with_400() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::new());
    let principal = Some(fixture_principal("delta", "seat-delta"));

    let uri = format!("{CAPTURE_EVENT_PATH}?cursor=not-a-cursor-token");
    let response = open_capture(&uri, principal.clone(), &hub, &port).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // The v2 fixture must be a version refusal, not a malformed one:
    // guard the fixture in-test before the route sees it.
    let v2_payload = "{\"version\":2,\"interest\":\"all\",\"coordinates\":[]}";
    let v2_token = base64url_encode(v2_payload.as_bytes());
    assert_eq!(
        CaptureCursor::decode(&v2_token, &CaptureInterest::All),
        Err(CaptureCursorError::WrongVersion { found: 2 })
    );
    let uri = format!("{CAPTURE_EVENT_PATH}?cursor={v2_token}");
    let response = open_capture(&uri, principal, &hub, &port).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(port.call_count(), 0);
}

#[tokio::test]
async fn a_wrong_location_cursor_refuses_with_409() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let port = Arc::new(PortDouble::new());
    let principal = Some(fixture_principal("delta", "seat-delta"));
    let other = interest_fixture("/work/two");
    let token = CaptureCursor::start(other).encode();
    let requested = interest_fixture("/work/one");
    // Guard: the token is sealed to the other location.
    assert_eq!(
        CaptureCursor::decode(&token, &requested),
        Err(CaptureCursorError::WrongLocation)
    );
    let uri = format!(
        "{CAPTURE_EVENT_PATH}?location[directory]={}&cursor={token}",
        pct("/work/one")
    );
    let response = open_capture(&uri, principal, &hub, &port).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(port.call_count(), 0);
}

#[tokio::test]
async fn a_refused_first_page_answers_409_and_503_by_failure_class() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let cases = [
        (
            "future coordinate",
            CapturePageError::Cursor(CaptureCursorError::FutureCoordinate {
                aggregate: session_id_fixture("ses_a"),
                seq: EventSeq::new(9),
                head: Some(EventSeq::new(5)),
            }),
            StatusCode::CONFLICT,
        ),
        (
            "stale aggregate",
            CapturePageError::Cursor(CaptureCursorError::Stale {
                aggregate: session_id_fixture("ses_b"),
            }),
            StatusCode::CONFLICT,
        ),
        (
            "corrupt row",
            CapturePageError::CorruptRow {
                id: dto_string_fixture("evt_corrupt"),
                detail: dto_string_fixture("the row does not decode"),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "unreadable store",
            CapturePageError::UnreadableStore {
                detail: dto_string_fixture("the store is offline"),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ];
    for (name, refusal, expected) in cases {
        let port = Arc::new(PortDouble::new());
        port.push_refusal(refusal);
        let response = open_capture(
            CAPTURE_EVENT_PATH,
            Some(fixture_principal("delta", "seat-delta")),
            &hub,
            &port,
        )
        .await;
        assert_eq!(response.status(), expected, "{name}");
        assert_eq!(
            port.call_count(),
            1,
            "{name}: the refusal is the first page itself"
        );
    }
}

#[tokio::test]
async fn an_interest_wider_than_the_bound_refuses_up_front() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    // 1025 distinct aggregates: one past the cursor's coordinate bound.
    // The refusal is the width probe itself: no page call, no stream.
    let port = Arc::new(PortDouble::new());
    port.push_width(1025);
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "the over-wide interest must refuse before the stream"
    );
    assert_eq!(port.call_count(), 0, "the width refusal makes no page call");

    // The bound-exact interest must stream: 1024 aggregates fit the
    // token, and the last page builds no continuation.
    let port = Arc::new(PortDouble::new());
    port.push_width(1024);
    port.push_page(page_fixture(vec![], None));
    let response = open_capture(
        CAPTURE_EVENT_PATH,
        Some(fixture_principal("delta", "seat-delta")),
        &hub,
        &port,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the bound-exact interest must stream"
    );
    let (_status, _content_type, mut body) = response_parts(response);
    let connected = frame_text(next_frame_ok(&mut body).await, "connected frame");
    assert!(connected.contains("\"type\":\"capture.connected\""));
    assert_pending(&mut body, "the bound-exact stream").await;
}

#[tokio::test]
async fn a_refused_width_probe_answers_by_failure_class() {
    let hub = hub_fixture(limits_fixture(4, MAX_SSE_FRAME_BYTES));
    let cases = [
        (
            "over-wide interest",
            CapturePageError::InterestExceedsBound { width: 2048 },
            StatusCode::CONFLICT,
        ),
        (
            "corrupt row",
            CapturePageError::CorruptRow {
                id: dto_string_fixture("evt_width"),
                detail: dto_string_fixture("the count row is corrupt"),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "unreadable store",
            CapturePageError::UnreadableStore {
                detail: dto_string_fixture("the store is offline"),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ];
    for (name, refusal, expected) in cases {
        let port = Arc::new(PortDouble::new());
        port.push_width_refusal(refusal);
        let response = open_capture(
            CAPTURE_EVENT_PATH,
            Some(fixture_principal("delta", "seat-delta")),
            &hub,
            &port,
        )
        .await;
        assert_eq!(response.status(), expected, "{name}");
        assert_eq!(
            port.call_count(),
            0,
            "{name}: the width probe precedes the page call"
        );
    }
}

#[path = "capture_journey_tests.rs"]
mod journey;
