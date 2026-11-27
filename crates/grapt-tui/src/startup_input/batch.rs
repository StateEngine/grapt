//! Publish a bounded decoded read together, preserving individual key semantics.
use crossterm::event::Event;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

pub const LIMIT: usize = 128;
pub type Packet = Vec<Result<Event, String>>;

pub struct Events {
    receiver: Receiver<Packet>,
    remaining: std::vec::IntoIter<Result<Event, String>>,
}

impl Events {
    pub fn new(receiver: Receiver<Packet>) -> Self {
        Self {
            receiver,
            remaining: Vec::new().into_iter(),
        }
    }

    pub fn try_recv(&mut self) -> Result<Result<Event, String>, TryRecvError> {
        if let Some(event) = self.remaining.next() {
            return Ok(event);
        }
        let packet = self.receiver.try_recv()?;
        self.remaining = packet.into_iter();
        self.remaining.next().ok_or(TryRecvError::Empty)
    }
}

pub struct Batch<'a> {
    output: &'a SyncSender<Packet>,
    wake: &'a SyncSender<()>,
    events: Packet,
}

impl<'a> Batch<'a> {
    pub fn new(output: &'a SyncSender<Packet>, wake: &'a SyncSender<()>) -> Self {
        Self {
            output,
            wake,
            events: Vec::with_capacity(LIMIT),
        }
    }

    pub fn push(&mut self, event: Result<Event, String>) -> bool {
        self.events.push(event);
        self.events.len() < LIMIT || self.flush()
    }

    pub fn flush(&mut self) -> bool {
        if self.events.is_empty() {
            return true;
        }
        let packet = std::mem::replace(&mut self.events, Vec::with_capacity(LIMIT));
        if self.output.send(packet).is_err() {
            return false;
        }
        let _ = self.wake.try_send(());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::mpsc;

    fn key(ch: char) -> Result<Event, String> {
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        )))
    }

    #[test]
    fn one_decoded_burst_is_published_atomically_without_becoming_paste() -> Result<(), String> {
        let (tx, rx) = mpsc::sync_channel(1);
        let (wake, notification) = mpsc::sync_channel(1);
        let mut events = Events::new(rx);
        let mut batch = Batch::new(&tx, &wake);
        for ch in ['q', 'k', '0'] {
            assert!(batch.push(key(ch)));
            assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        }
        assert!(batch.flush());
        notification.try_recv().map_err(|e| e.to_string())?;
        for ch in ['q', 'k', '0'] {
            assert_eq!(events.try_recv().map_err(|e| e.to_string())?, key(ch));
        }
        assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
        Ok(())
    }

    #[test]
    fn long_reads_flush_bounded_chunks_and_keep_the_tail_on_disconnect() -> Result<(), String> {
        let (tx, rx) = mpsc::sync_channel(2);
        let (wake, _notification) = mpsc::sync_channel(1);
        let mut batch = Batch::new(&tx, &wake);
        for _ in 0..LIMIT + 1 {
            assert!(batch.push(key('x')));
        }
        let first = rx.try_recv().map_err(|e| e.to_string())?;
        assert_eq!(first.len(), LIMIT);
        assert!(batch.flush());
        drop(batch);
        drop(tx);
        let mut events = Events::new(rx);
        assert_eq!(events.try_recv().map_err(|e| e.to_string())?, key('x'));
        assert!(matches!(events.try_recv(), Err(TryRecvError::Disconnected)));
        Ok(())
    }

    #[test]
    fn a_stopped_consumer_is_reported_and_empty_flush_does_not_wake() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (wake, notification) = mpsc::sync_channel(1);
        let mut batch = Batch::new(&tx, &wake);
        assert!(batch.flush());
        assert!(notification.try_recv().is_err());
        drop(rx);
        assert!(batch.push(key('x')));
        assert!(!batch.flush());
    }
}
