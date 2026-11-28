//! Committed hub frames as seat commands. The seat is busy while any session
//! is busy, and a succeeded execution becomes one turn with the text it
//! produced. Failed and interrupted executions publish nothing.
use super::publisher::{Body, Command, SeatTurn};
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

struct Draft {
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
    pub(super) fn observe(&mut self, frame: &[u8]) -> Vec<Command> {
        let Some(event) = event(frame) else {
            return Vec::new();
        };
        let data = &event["data"];
        let Some(session) = data["sessionID"].as_str().map(str::to_owned) else {
            return Vec::new();
        };
        match event["type"].as_str() {
            Some("session.status") => self.status(session, data["status"]["type"].as_str()),
            Some("session.step.started") => {
                if let Some(draft) = self.draft(&session) {
                    draft.provider = text(&data["model"]["providerID"]);
                    draft.model = text(&data["model"]["id"]);
                }
                Vec::new()
            }
            Some("session.text.ended") => {
                if let Some(draft) = self.draft(&session) {
                    draft.slice(data);
                }
                Vec::new()
            }
            Some("session.execution.succeeded") => {
                let vendor = data["executionID"].as_str().map(str::to_owned);
                let committed = committed_us(&event["created"]);
                let draft = self.turns.remove(&session);
                match (vendor, committed, draft) {
                    (Some(vendor), Some(committed), Some(draft)) => draft
                        .finish(session, vendor, committed)
                        .map(Command::Turn)
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                }
            }
            Some("session.execution.failed" | "session.execution.interrupted") => {
                self.turns.remove(&session);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The idle report a busy seat owes when its event feed ends: without it
    /// the host keeps holding the seat's mail.
    pub(super) fn release(&mut self) -> Option<Command> {
        self.turns.clear();
        (!std::mem::take(&mut self.busy).is_empty()).then_some(Command::Activity(Activity::Idle))
    }

    fn status(&mut self, session: String, status: Option<&str>) -> Vec<Command> {
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
            (false, false) => vec![Command::Activity(Activity::Busy)],
            (true, true) => vec![Command::Activity(Activity::Idle)],
            _ => Vec::new(),
        }
    }

    fn draft(&mut self, session: &str) -> Option<&mut Draft> {
        if !self.turns.contains_key(session) && self.turns.len() >= MAX_SESSIONS {
            return None;
        }
        Some(
            self.turns
                .entry(session.to_owned())
                .or_insert_with(Draft::new),
        )
    }
}

impl Draft {
    fn new() -> Self {
        Self {
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
    fn finish(
        self,
        conversation: String,
        vendor_turn: String,
        occurred_at_us: i64,
    ) -> Option<SeatTurn> {
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
            vendor_turn,
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
