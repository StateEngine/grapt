//! Resume custody (B3 S3b) shared by the bridge and its reader: the offer the
//! client made for the next subscribe, how many frames past it the client
//! already holds, and what the reader built with them. The bridge adopts a
//! build's binding only for the attachment it installs, holds that install's
//! refreshes until the client answers the first frame, and the reader leaves
//! the replayed prefix's per-frame refreshes alone.
use super::*;
use grapt_client::client::{ResumeToken, StreamBinding};
use std::sync::{Mutex, MutexGuard};

/// The custody both threads share.
#[derive(Debug, Default)]
pub(super) struct Custody {
    offer: Option<ResumeToken>,
    suffix: usize,
    /// The newest build: its attachment's validity, the binding its stream
    /// was opened under, and whether that subscribe carried the offer.
    built: Option<(Arc<AtomicBool>, StreamBinding, bool)>,
}

pub(super) type SharedCustody = Arc<Mutex<Custody>>;

/// A poisoned lock still holds the last custody either thread wrote.
fn held(custody: &SharedCustody) -> MutexGuard<'_, Custody> {
    custody
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The reader's per-frame suppression for one build: after the certificate
/// id, the next `remaining` positioned frames raise nothing.
#[derive(Debug, Default)]
pub(super) struct Suppress {
    certificate: Option<String>,
    remaining: usize,
}

/// The install refreshes an offered build holds back: none, held with its
/// first frame still to pass the catalog hold, or held with that frame
/// delivered and the client's answer still to come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    None,
    FirstFrame,
    Delivered,
}

/// What a build starts from: the current offer and the client's suffix.
pub(super) fn offer(custody: &SharedCustody) -> (Option<ResumeToken>, usize) {
    let custody = held(custody);
    (custody.offer.clone(), custody.suffix)
}

/// Record a verified build, keyed by its attachment's validity, and return
/// the suppression its frames get. A subscribe carried the offer only when
/// its binding is the token's; only such a build may suppress a replayed prefix.
pub(super) fn record(
    custody: &SharedCustody,
    valid: &Arc<AtomicBool>,
    stream: &HttpConnection,
    offer: Option<ResumeToken>,
    suffix: usize,
) -> Suppress {
    let binding = stream.stream_binding();
    let offered = offer
        .as_ref()
        .is_some_and(|token| Some(&token.binding) == binding.as_ref());
    held(custody).built = binding.map(|binding| (Arc::clone(valid), binding, offered));
    Suppress {
        certificate: offer.filter(|_| offered).map(|token| token.id),
        remaining: if offered { suffix } else { 0 },
    }
}

/// The refreshes one frame raises: (permissions, session catalog). Only the
/// first frame of an offered build can be the certificate; after it, the
/// replayed suffix raises nothing, and frames beyond it raise as live.
pub(super) fn frame_flags(id: Option<&str>, text: &str, suppress: &mut Suppress) -> (bool, bool) {
    if let Some(certificate) = suppress.certificate.take() {
        if id == Some(certificate.as_str()) {
            return (false, false);
        }
        suppress.remaining = 0;
    } else if suppress.remaining > 0 && id.is_some() {
        suppress.remaining -= 1;
        return (false, false);
    }
    (authority_changed(text), catalog_changed(text))
}

impl Bridge {
    /// A verified replacement was installed: adopt its build's binding only
    /// when the build is this attachment's. A build that carried the offer
    /// holds the install's refreshes until the first frame is answered, and
    /// that frame passes the catalog hold; any other raises them as always.
    pub(super) fn adopt_custody(&mut self) {
        let built = held(&self.custody)
            .built
            .take()
            .filter(|(valid, ..)| Arc::ptr_eq(valid, &self.fence));
        let offered = built.as_ref().is_some_and(|(.., offered)| *offered);
        self.binding = built.map(|(_, binding, _)| binding);
        if offered {
            self.answer = Answer::FirstFrame;
        } else {
            self.answer = Answer::None;
            self.raise_refreshes();
        }
    }

    /// The open stream's binding, only while it is the installed, unretired,
    /// unfenced attachment and no replacement is in progress.
    pub(super) fn bound_stream(&self) -> Option<StreamBinding> {
        if self.replacement_requested || self.retired || !self.fence.load(Ordering::Acquire) {
            return None;
        }
        self.binding.clone()
    }

    /// Offer or withdraw a resume for the next build. Withdrawing a held
    /// offer while a replacement is in progress advances the generation, so
    /// a build that used the token is refused as stale and rebuilt fresh.
    pub(super) fn offer(&mut self, token: Option<ResumeToken>) {
        let withdrawn = {
            let mut custody = held(&self.custody);
            let withdrawn = token.is_none() && custody.offer.is_some();
            custody.offer = token;
            withdrawn
        };
        if withdrawn && self.replacement_requested {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(super) fn offer_suffix(&mut self, frames: usize) {
        held(&self.custody).suffix = frames;
    }

    /// The client answered the first frame: the offer is consumed, and held
    /// install refreshes are dropped when certified or raised when not.
    pub(super) fn answered(&mut self, certified: bool) {
        held(&self.custody).offer = None;
        if std::mem::replace(&mut self.answer, Answer::None) != Answer::None && !certified {
            self.raise_refreshes();
        }
    }

    /// The watched Session changed: no offer survives it, and held install
    /// refreshes are raised once.
    pub(super) fn session_changed(&mut self) {
        held(&self.custody).offer = None;
        if std::mem::replace(&mut self.answer, Answer::None) != Answer::None {
            self.raise_refreshes();
        }
    }

    fn raise_refreshes(&self) {
        self.authority_dirty.store(true, Ordering::Release);
        self.sessions_dirty.store(true, Ordering::Release);
    }
}

/// Every permission event (asked, replied, cancelled, suspended, a revoked
/// rule) changes what the operator may approve. Matched by event type: a
/// streamed delta that merely mentions a permission must not trigger a
/// synchronous refresh.
pub(super) fn authority_changed(text: &str) -> bool {
    text.contains("\"permission.")
        && event_type(text).is_some_and(|kind| kind.starts_with("permission."))
}

pub(super) fn catalog_changed(text: &str) -> bool {
    // Starts discover new children; completion and usage refresh their reported
    // counters. Streaming delta frames do not trigger catalog reads.
    const KINDS: [&str; 8] = [
        "session.created",
        "session.updated",
        "session.title.changed",
        "session.execution.started",
        "session.execution.succeeded",
        "session.execution.failed",
        "session.execution.interrupted",
        "session.usage.updated",
    ];
    KINDS.iter().any(|kind| text.contains(kind))
        && event_type(text).is_some_and(|kind| KINDS.contains(&kind.as_str()))
}

/// The frame's event type, read only after a cheap substring prefilter.
fn event_type(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("type")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn first_frame(bridge: &mut Bridge) -> std::result::Result<FramePoll, TransportError> {
        let mut observed = Ok(FramePoll::Idle);
        until(|| {
            observed = bridge.poll_frame();
            !matches!(observed, Ok(FramePoll::Idle))
        });
        observed
    }

    #[test]
    fn fresh_builds_never_suppress_a_retained_suffix() -> TestResult {
        let profile = Profile::new()?;
        let peer = Peer::start("old")?;
        peer.window_requests.positioned(true);
        profile.publish(&peer, false)?;
        let (_, mut stream) = profile.config()?.connect(Bounds::PRODUCTION)?;
        stream.resubscribe();
        let custody = SharedCustody::default();
        let valid = Arc::new(AtomicBool::new(true));
        let mismatch = ResumeToken {
            id: "fixture-1".into(),
            binding: StreamBinding::new("another endpoint", 0, None),
        };
        for offer in [None, Some(mismatch)] {
            let mut suppress = record(&custody, &valid, &stream, offer, 3);
            for (text, flags) in [
                (r#"{"type":"permission.asked"}"#, (true, false)),
                (r#"{"type":"session.updated"}"#, (false, true)),
            ] {
                assert_eq!(frame_flags(Some("fresh-2"), text, &mut suppress), flags);
            }
        }
        let offer = ResumeToken {
            id: "fixture-1".into(),
            binding: stream.stream_binding().ok_or("binding")?,
        };
        let mut suppress = record(&custody, &valid, &stream, Some(offer), 3);
        let text = r#"{"type":"permission.asked"}"#;
        for id in ["fixture-1", "fixture-2", "fixture-3", "fixture-4"] {
            assert_eq!(frame_flags(Some(id), text, &mut suppress), (false, false));
        }
        assert_eq!(
            frame_flags(Some("fixture-5"), text, &mut suppress),
            (true, false)
        );
        Ok(())
    }

    #[test]
    fn native_certificate_passes_the_catalog_hold_and_drops_install_refreshes() -> TestResult {
        let profile = Profile::new()?;
        let peer = Peer::start("old")?;
        peer.window_requests.positioned(true);
        profile.publish(&peer, false)?;
        let mut bridge = opened(&profile)?;
        assert!(matches!(
            first_frame(&mut bridge)?,
            FramePoll::Positioned { .. }
        ));
        let binding = bridge.stream_binding().ok_or("binding")?;
        bridge.authority_dirty.store(false, Ordering::Release);
        bridge.sessions_dirty.store(false, Ordering::Release);
        bridge.resume_from(Some(ResumeToken {
            id: "fixture-1".into(),
            binding,
        }));
        bridge.resume_suffix(0);
        bridge.retire_stream();
        assert_eq!(reopen(&mut bridge)?, Some(true));
        assert!(!bridge.authority_dirty.load(Ordering::Acquire));
        assert!(!bridge.sessions_dirty.load(Ordering::Acquire));
        bridge.sessions_dirty.store(true, Ordering::Release);
        assert!(
            matches!(first_frame(&mut bridge)?, FramePoll::Positioned { id, .. } if id == "fixture-1")
        );
        assert!(
            bridge.sessions_dirty.swap(false, Ordering::AcqRel),
            "queued catalog work survives"
        );
        bridge.resume_answered(true);
        assert!(!bridge.authority_dirty.load(Ordering::Acquire));
        assert!(!bridge.sessions_dirty.load(Ordering::Acquire));
        let headers = peer.window_requests.headers()?;
        assert_eq!(headers.len(), 2);
        assert!(!headers[0].contains("Last-Event-ID:"));
        assert!(headers[1].contains("Last-Event-ID: fixture-1\r\n"));
        assert_eq!(
            peer.calls.load(Ordering::Acquire),
            0,
            "certification starts no RPC"
        );
        Ok(())
    }

    #[test]
    fn native_withdrawal_fences_a_delayed_offered_build_and_rebuilds_fresh() -> TestResult {
        let profile = Profile::new()?;
        let peer = Peer::start("old")?;
        peer.window_requests.positioned(true);
        profile.publish(&peer, false)?;
        let mut bridge = opened(&profile)?;
        assert!(matches!(
            first_frame(&mut bridge)?,
            FramePoll::Positioned { .. }
        ));
        let binding = bridge.stream_binding().ok_or("binding")?;
        peer.window_requests.hold();
        bridge.resume_from(Some(ResumeToken {
            id: "fixture-1".into(),
            binding,
        }));
        bridge.retire_stream();
        until(|| peer.events.load(Ordering::Acquire) == 2);
        assert_eq!(
            peer.events.load(Ordering::Acquire),
            2,
            "offered request reached peer"
        );
        let generation = bridge.generation.load(Ordering::Acquire);
        bridge.resume_from(None);
        assert!(bridge.generation.load(Ordering::Acquire) > generation);
        peer.window_requests.release();
        assert_eq!(reopen(&mut bridge)?, Some(true));
        let headers = peer.window_requests.headers()?;
        assert_eq!(headers.len(), 3);
        assert!(headers[1].contains("Last-Event-ID: fixture-1\r\n"));
        assert!(!headers[2].contains("Last-Event-ID:"));
        assert!(bridge.authority_dirty.swap(false, Ordering::AcqRel));
        assert!(bridge.sessions_dirty.swap(false, Ordering::AcqRel));
        assert!(matches!(
            first_frame(&mut bridge)?,
            FramePoll::Positioned { .. }
        ));
        bridge.resume_answered(false);
        bridge.resume_answered(false);
        assert!(!bridge.authority_dirty.load(Ordering::Acquire));
        assert!(!bridge.sessions_dirty.load(Ordering::Acquire));
        Ok(())
    }
}
