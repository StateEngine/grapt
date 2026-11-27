//! Tokio 1.53.1 owns SetConsoleCtrlHandler and its unsafe callback. Its
//! terminating-event callback stays parked until we exit or Windows' timeout.
use crate::Event;
use std::{
    sync::{mpsc::SyncSender, Arc},
    task::{Context, Poll, Wake, Waker},
};
use tokio::signal::windows;

struct Notify(SyncSender<()>);

impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}

#[derive(Debug)]
pub struct Listener {
    control_c: windows::CtrlC,
    control_break: windows::CtrlBreak,
    close: windows::CtrlClose,
    logoff: windows::CtrlLogoff,
    shutdown: windows::CtrlShutdown,
    waker: Waker,
}

impl Listener {
    pub fn new(wake: SyncSender<()>) -> Result<Self, String> {
        (|| -> std::io::Result<Self> {
            Ok(Self {
                control_c: windows::ctrl_c()?,
                control_break: windows::ctrl_break()?,
                close: windows::ctrl_close()?,
                logoff: windows::ctrl_logoff()?,
                shutdown: windows::ctrl_shutdown()?,
                waker: Waker::from(Arc::new(Notify(wake))),
            })
        })()
        .map_err(|error| format!("Cannot register console control handler: {error}"))
    }

    pub fn poll(&mut self) -> Option<Event> {
        let context = &mut Context::from_waker(&self.waker);
        // An unavoidable exit takes priority over an interrupt notification.
        if matches!(self.close.poll_recv(context), Poll::Ready(Some(()))) {
            Some(Event::Close)
        } else if matches!(self.logoff.poll_recv(context), Poll::Ready(Some(()))) {
            Some(Event::Logoff)
        } else if matches!(self.shutdown.poll_recv(context), Poll::Ready(Some(()))) {
            Some(Event::Shutdown)
        } else if matches!(self.control_break.poll_recv(context), Poll::Ready(Some(()))) {
            Some(Event::Break)
        } else if matches!(self.control_c.poll_recv(context), Poll::Ready(Some(()))) {
            Some(Event::ControlC)
        } else {
            None
        }
    }
}
