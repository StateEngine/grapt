//! Seats end on their owner's EOF or signal; explicit fixture serves use a timer.
//! The prompt driver drains work before the service releases listener custody.

use std::io::{Read, Write};

mod feedback;
pub(super) use feedback::redact;

#[derive(Debug)]
pub(super) enum Lifetime {
    Window,
    Service,
    Terminal {
        control: tokio::sync::oneshot::Receiver<Result<(), String>>,
        notify_ready: bool,
    },
}

impl Lifetime {
    pub(super) fn platform(&self, error: grapt_platform::PlatformRefusal) -> super::ProductRefusal {
        match self {
            Self::Window | Self::Service => crate::installation::platform_refusal(error),
            Self::Terminal { .. } => feedback::platform(error),
        }
    }

    pub(super) fn terminal(notify_ready: bool) -> Result<Self, super::ProductRefusal> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("terminal-control".into())
            .spawn(move || {
                let mut byte = [0; 1];
                let result = match std::io::stdin().lock().read(&mut byte) {
                    Ok(0) => Ok(()),
                    Ok(_) => Err("terminal control pipe contained unexpected input".into()),
                    Err(error) => Err(format!(
                        "terminal control pipe is unreadable: {:?}",
                        error.kind()
                    )),
                };
                let _receiver_closed = sender.send(result);
            })
            .map_err(|error| {
                super::ProductRefusal::Compose(format!(
                    "terminal control owner failed: {:?}",
                    error.kind()
                ))
            })?;
        Ok(Self::Terminal {
            control: receiver,
            notify_ready,
        })
    }

    fn notify_ready(&self, output: &mut impl Write) -> Result<(), String> {
        if matches!(
            self,
            Self::Terminal {
                notify_ready: true,
                ..
            }
        ) {
            // Private opt-in stdout framing: one RS byte precedes the final
            // launch receipt. This wakes discovery; it grants no authority.
            // `until` is entered only after registration and ready publication.
            write_notification(output)?;
        }
        Ok(())
    }

    pub(super) async fn closed(self, window_ms: u64) -> Result<(), String> {
        match self {
            Self::Window => {
                tokio::time::sleep(std::time::Duration::from_millis(window_ms)).await;
                Ok(())
            }
            Self::Service => service_signal().await,
            Self::Terminal {
                control: receiver, ..
            } => receiver
                .await
                .map_err(|_| "terminal control owner disappeared".to_owned())?,
        }
    }
}

pub fn run_service(
    service_root: &std::path::Path,
) -> Result<crate::receipt::LaunchReceipt, super::ProductRefusal> {
    super::run_lifetime(service_root, Lifetime::Service, super::HostMode::Standalone)
}

async fn service_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .map_err(|error| format!("service termination observer failed: {:?}", error.kind()))?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|error| format!("service cancellation observer failed: {:?}", error.kind())),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("service cancellation observer failed: {:?}", error.kind()))
}

pub(super) async fn until(
    lifetime: Lifetime,
    window_ms: u64,
    driver: Option<&mut super::ServiceDriver>,
) -> Option<String> {
    if let Err(refusal) = lifetime.notify_ready(&mut std::io::stdout().lock()) {
        // The caller still drains and retires its published registration.
        return Some(refusal);
    }
    crate::diagnostics::report(format!(
        "service lifetime={} configured_fixture_window_ms={window_ms}",
        match &lifetime {
            Lifetime::Window => "bounded-fixture",
            Lifetime::Service => "service-unlimited",
            Lifetime::Terminal { .. } => "terminal-owned-unlimited",
        }
    ));
    let reason = match &lifetime {
        Lifetime::Window => crate::dogfood::lifecycle::StopReason::ServeWindowExpired,
        Lifetime::Service => crate::dogfood::lifecycle::StopReason::ServiceStopped,
        Lifetime::Terminal { .. } => crate::dogfood::lifecycle::StopReason::TerminalClosed,
    };
    let mut control_refusal = None;
    let stop = async {
        control_refusal = lifetime.closed(window_ms).await.err();
        crate::diagnostics::report(format!("service stopping reason={}", reason.as_str()));
        reason
    };
    let refusal = match driver {
        Some(driver) => driver.run_until_shutdown(stop).await.err(),
        None => {
            stop.await;
            None
        }
    };
    refusal.or(control_refusal)
}

fn write_notification(output: &mut impl Write) -> Result<(), String> {
    output
        .write_all(b"\x1e")
        .and_then(|()| output.flush())
        .map_err(|error| format!("terminal readiness notification failed: {:?}", error.kind()))
}

#[cfg(test)]
mod tests {
    use super::Lifetime;
    use std::io::{self, Write};

    #[test]
    fn only_opted_in_terminal_lifetimes_emit_the_readiness_byte() -> Result<(), String> {
        let mut output = Vec::new();
        Lifetime::Window.notify_ready(&mut output)?;
        for notify_ready in [false, true] {
            let (_sender, control) = tokio::sync::oneshot::channel();
            Lifetime::Terminal {
                control,
                notify_ready,
            }
            .notify_ready(&mut output)?;
            assert_eq!(output, if notify_ready { &b"\x1e"[..] } else { &b""[..] });
        }
        Ok(())
    }

    #[test]
    fn readiness_requires_a_successful_flush() {
        struct BrokenFlush;
        impl Write for BrokenFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let error = super::write_notification(&mut BrokenFlush);
        assert_eq!(
            error,
            Err("terminal readiness notification failed: BrokenPipe".into())
        );
    }

    #[tokio::test]
    async fn notified_terminal_still_closes_on_private_control_eof() -> Result<(), String> {
        let (sender, control) = tokio::sync::oneshot::channel();
        sender
            .send(Ok(()))
            .map_err(|_| "control receiver disappeared")?;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            Lifetime::Terminal {
                control,
                notify_ready: true,
            }
            .closed(30_000),
        )
        .await
        .map_err(|_| "notification changed private EOF handling")??;
        Ok(())
    }

    #[tokio::test]
    async fn frc_control_eof_closes_before_the_serve_window() -> Result<(), String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(Ok(()))
            .map_err(|_| "control receiver disappeared".to_owned())?;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            Lifetime::Terminal {
                control: receiver,
                notify_ready: false,
            }
            .closed(30_000),
        )
        .await
        .map_err(|_| "control EOF was ignored".to_owned())??;
        Ok(())
    }

    #[tokio::test]
    async fn frc_unreadable_control_is_not_a_clean_exit() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sent = sender.send(Err("control unreadable".into()));
        assert!(sent.is_ok());
        assert_eq!(
            Lifetime::Terminal {
                control: receiver,
                notify_ready: false
            }
            .closed(30_000)
            .await,
            Err("control unreadable".into())
        );
    }

    #[tokio::test]
    async fn frc_control_owner_loss_is_not_eof() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        drop(sender);
        assert_eq!(
            Lifetime::Terminal {
                control: receiver,
                notify_ready: false
            }
            .closed(30_000)
            .await,
            Err("terminal control owner disappeared".into())
        );
    }
}

#[cfg(test)]
mod lifetime_regression {
    use super::Lifetime;
    #[tokio::test(start_paused = true)]
    async fn terminal_owner_outlives_the_configured_fixture_window() -> Result<(), String> {
        let (sender, control) = tokio::sync::oneshot::channel();
        let mut closed = Box::pin(
            Lifetime::Terminal {
                control,
                notify_ready: false,
            }
            .closed(1),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(120), &mut closed)
                .await
                .is_err()
        );
        sender
            .send(Ok(()))
            .map_err(|_| "terminal owner was discarded")?;
        closed.await
    }
}
