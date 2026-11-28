//! Committed hub frames as seat reports. The seat is busy while any session
//! is busy, and a succeeded execution becomes one turn with the text it
//! produced. Failed and interrupted executions publish nothing, and neither
//! does one whose start this subscription did not see: its text is partial.
use super::publisher::{Body, SeatTurn};
use grapt_stateengine_ipc::{Activity, Sha256Hex};
use ring::digest::{Context, SHA256};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Sessions followed at once; beyond it a session's turns are not published.
const MAX_SESSIONS: usize = 256;
/// Text kept for one turn; past it the body is reported by size and digest.
const MAX_BODY_BYTES: usize = 1_048_576;
const SEPARATOR: &str = "\n\n";

#[derive(Default)]
pub(super) struct Tracker {
    busy: HashSet<String>,
    turns: HashMap<String, Draft>,
}

/// What the hub's committed events say the seat should report.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Report {
    Activity(Activity),
    Turn(SeatTurn),
}

struct Draft {
    /// The execution whose committed start opened this draft.
    execution: String,
    provider: String,
    model: String,
    text: Option<String>,
    digest: Context,
    bytes: u64,
    slices: u32,
    /// The digest of the one elided slice that is the whole body, if any.
    elided: Option<Sha256Hex>,
    /// An elided slice beside others: the body's digest cannot be known.
    unknowable: bool,
}

impl Tracker {
    pub(super) fn observe(&mut self, frame: &[u8]) -> Option<Report> {
        let event = event(frame)?;
        let data = &event["data"];
        let session = data["sessionID"].as_str()?.to_owned();
        let execution = data["executionID"].as_str();
        match event["type"].as_str()? {
            "session.status" => self.status(session, data["status"]["type"].as_str()),
            "session.execution.started" => {
                if self.turns.len() < MAX_SESSIONS || self.turns.contains_key(&session) {
                    let draft = Draft::new(execution?.to_owned());
                    self.turns.insert(session, draft);
                }
                None
            }
            "session.step.started" => {
                let draft = self.turns.get_mut(&session)?;
                draft.provider = text(&data["model"]["providerID"]);
                draft.model = text(&data["model"]["id"]);
                None
            }
            "session.text.ended" => {
                self.turns.get_mut(&session)?.slice(data);
                None
            }
            "session.execution.succeeded" => {
                let draft = self.turns.remove(&session)?;
                if execution != Some(draft.execution.as_str()) {
                    return None;
                }
                draft
                    .finish(session, committed_us(&event["created"])?)
                    .map(Report::Turn)
            }
            "session.execution.failed" | "session.execution.interrupted" => {
                self.turns.remove(&session);
                None
            }
            _ => None,
        }
    }

    /// Forget every session, saying whether any was busy.
    pub(super) fn release(&mut self) -> bool {
        self.turns.clear();
        !std::mem::take(&mut self.busy).is_empty()
    }

    fn status(&mut self, session: String, status: Option<&str>) -> Option<Report> {
        let was_busy = !self.busy.is_empty();
        match status {
            Some("idle") => {
                self.busy.remove(&session);
            }
            Some(_) if self.busy.len() < MAX_SESSIONS || self.busy.contains(&session) => {
                self.busy.insert(session);
            }
            _ => {}
        }
        match (was_busy, self.busy.is_empty()) {
            (false, false) => Some(Report::Activity(Activity::Busy)),
            (true, true) => Some(Report::Activity(Activity::Idle)),
            _ => None,
        }
    }
}

impl Draft {
    fn new(execution: String) -> Self {
        Self {
            execution,
            provider: String::new(),
            model: String::new(),
            text: Some(String::new()),
            digest: Context::new(&SHA256),
            bytes: 0,
            slices: 0,
            elided: None,
            unknowable: false,
        }
    }

    fn slice(&mut self, data: &Value) {
        let separator = if self.slices == 0 { "" } else { SEPARATOR };
        self.slices += 1;
        let elided = data["textSha256"].as_str().and_then(Sha256Hex::parse);
        let Some(elided) = elided else {
            self.append(separator, data["text"].as_str().unwrap_or_default());
            return;
        };
        let bytes = data["textBytes"].as_u64().unwrap_or_default();
        self.unknowable |= self.slices > 1;
        self.elided = (self.slices == 1).then_some(elided);
        self.bytes = self.bytes.saturating_add(bytes);
        self.text = None;
    }

    fn append(&mut self, separator: &str, text: &str) {
        self.unknowable |= self.elided.is_some();
        for part in [separator, text] {
            self.digest.update(part.as_bytes());
            self.bytes = self.bytes.saturating_add(part.len() as u64);
        }
        if let Some(body) = &mut self.text {
            if body.len() + separator.len() + text.len() <= MAX_BODY_BYTES {
                body.push_str(separator);
                body.push_str(text);
            } else {
                self.text = None;
            }
        }
    }

    /// The turn, or `None` when it produced no text or its digest is unknown.
    fn finish(self, conversation: String, occurred_at_us: i64) -> Option<SeatTurn> {
        if self.slices == 0 || self.unknowable {
            return None;
        }
        let digest = match self.elided {
            Some(elided) => elided,
            None => {
                let digest = self.digest.finish();
                Sha256Hex(digest.as_ref().try_into().ok()?)
            }
        };
        let body = match self.text {
            Some(text) => Body::Whole(text),
            None => Body::Omitted(self.bytes),
        };
        Some(SeatTurn {
            conversation,
            vendor_turn: self.execution,
            occurred_at_us,
            provider: self.provider,
            model: self.model,
            body,
            digest,
        })
    }
}

/// A committed frame's `created` milliseconds, as microseconds.
fn committed_us(created: &Value) -> Option<i64> {
    let millis = created
        .as_f64()
        .filter(|millis| millis.is_finite() && *millis >= 0.0)?;
    format!("{millis:.0}")
        .parse::<i64>()
        .ok()?
        .checked_mul(1000)
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// The JSON event in one SSE frame.
fn event(frame: &[u8]) -> Option<Value> {
    let marker = b"data: ";
    let start = frame
        .windows(marker.len())
        .position(|window| window == marker)?
        + marker.len();
    let data = frame.get(start..)?;
    let end = data
        .windows(2)
        .position(|window| window == b"\n\n")
        .unwrap_or(data.len());
    serde_json::from_slice(data.get(..end)?).ok()
}
