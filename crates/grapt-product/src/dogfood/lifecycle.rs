// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Shutdown fencing/reporting.
// Modified for the existing execution registry, managed cancellation and hub.
//! The owning service names shutdown; provider cancellation never invents it.
use grapt_types::{ClockAuthority, IdAuthority};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    ServeWindowExpired,
    ServiceStopped,
    TerminalClosed,
}
impl StopReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ServeWindowExpired => "serve-window-expired",
            Self::ServiceStopped => "service-stopped",
            Self::TerminalClosed => "terminal-closed",
        }
    }
    pub(super) fn durable(self) -> grapt_session::execution::InterruptionReason {
        match self {
            Self::ServeWindowExpired => {
                grapt_session::execution::InterruptionReason::ServeWindowExpired
            }
            _ => grapt_session::execution::InterruptionReason::Shutdown,
        }
    }
    /// Why a service stop should let the turn continue later, or none when the
    /// stop was the user's own decision.
    ///
    /// A service restart is not a statement about the work, so its turn stays
    /// continuable. A terminal close is the user closing the session (F23), and
    /// it stays closed; it is never recorded as resumable.
    pub(super) fn resume_cause(self) -> Option<grapt_session::interrupted_turn::StoppedBy> {
        match self {
            Self::ServiceStopped => {
                Some(grapt_session::interrupted_turn::StoppedBy::ServiceShutdown)
            }
            Self::ServeWindowExpired => {
                Some(grapt_session::interrupted_turn::StoppedBy::ServeWindowExpired)
            }
            Self::TerminalClosed => None,
        }
    }
}
impl super::ExecutionBackend {
    pub(super) fn shutdown(
        &self,
        session: &grapt_types::SessionId,
        execution: &grapt_types::ExecutionId,
        reason: StopReason,
    ) -> Result<(), String> {
        self.0
            .shutdown(session, execution, reason)
            .map_err(|error| error.to_string())
    }
}

pub(super) fn attach_notice(
    driver: &mut super::prompt_driver::PromptDriver,
    application: &super::DogfoodApplication,
) {
    let ids = application.ids.clone();
    let clock = application.clock.clone();
    let hub = application.hub.clone();
    driver.attach_shutdown_notice(std::sync::Arc::new(move |reason| {
        announce(ids.as_ref(), clock.as_ref(), &hub, reason);
    }));
}

/// Stop a Session's turn because its last window is gone.
///
/// It takes the same path an explicit user stop takes, so a close that lands
/// while the turn is still preparing, or while admitted input waits behind it,
/// stops as completely as Esc would. A `Weak` application keeps this from
/// holding the product alive, and a stop that arrives after teardown has
/// nothing left to stop, so it is dropped rather than reported as a failure.
///
/// A Session launched headless is EXEMPT: its launch said it may run with no
/// window, so a window that attached and left must not stop it. The answer
/// comes from the cached launch modes, never from a store read — this runs on
/// the deadline loop, and a slow store there would stall every other Session's
/// grace.
pub(super) fn attach_close_grace(
    driver: &mut super::prompt_driver::PromptDriver,
    application: &std::sync::Arc<super::DogfoodApplication>,
) {
    let weak = std::sync::Arc::downgrade(application);
    let windows = std::sync::Arc::clone(application.hub.windows());
    driver.attach_close_grace(
        windows,
        std::sync::Arc::new(move |session: &grapt_types::SessionId| {
            let Some(application) = weak.upgrade() else {
                return;
            };
            if application.launches.is_headless(session) {
                // Keeping the turn is the deliberate outcome of the launch
                // fact, not a failure to stop it.
                return;
            }
            // Stopping is not enough on its own: the close must also hold later
            // native wakes, or a mail delivery restarts the turn with no window
            // left to watch it.
            application.close_after_last_window(session);
        }),
    );
}

fn announce(
    ids: &dyn IdAuthority,
    clock: &dyn ClockAuthority,
    hub: &grapt_api::DogfoodEventHub,
    reason: StopReason,
) {
    let event = (|| -> Result<_, String> {
        let event: grapt_types::EventId = format!("evt_{}", ids.mint().as_str())
            .parse()
            .map_err(|_| "shutdown notice identity was refused")?;
        let id = grapt_api::u3a::WireId::parse(event.as_str().to_owned())
            .ok_or("shutdown notice identity was refused")?;
        let created = grapt_api::u3a::FiniteF64::try_new(clock.now().as_millis() as f64)
            .map_err(|error| error.to_string())?;
        let reason = grapt_api::u3a::BoundedString::try_new(reason.as_str().to_owned())
            .map_err(|error| error.to_string())?;
        Ok(grapt_api::u3a::DogfoodEvent::ServerStopping(
            grapt_api::u3a::LiveEvent::new(
                id,
                created,
                grapt_api::u3a::ServerStoppingData { reason },
            ),
        ))
    })();
    if let Err(error) = event.and_then(|event| {
        hub.publish(&event)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        crate::diagnostics::report(format!(
            "service shutdown notice could not be published: {error}"
        ));
    }
}
