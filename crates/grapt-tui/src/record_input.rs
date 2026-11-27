//! Bridge bounded console records to the shared parser's input-wait policy.
use crate::input_bytes::Parser;
use crossterm::event::Event;
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError},
    time::Duration,
};

type PollResult<T> = Result<(Option<T>, Vec<Result<Event, String>>), String>;

// Adapted from crossterm-rs/crossterm, src/event/source/windows.rs,
// commit 36d95b26a26e64b0f8c12edfe11f410a6d56a812.
// Copyright (c) 2019 Timon. MIT; see licenses/crossterm/LICENSE.
// Modified by Grapt: share the native focus conversion with notification tests.
pub fn focus(focused: bool) -> Event {
    if focused {
        Event::FocusGained
    } else {
        Event::FocusLost
    }
}

// Function-key conversion uses the same upstream at src/event/sys/windows/parse.rs.
pub fn function_key(virtual_key: u16) -> Option<crossterm::event::KeyCode> {
    if !(112..=135).contains(&virtual_key) {
        return None;
    }
    Some(crossterm::event::KeyCode::F(
        u8::try_from(virtual_key - 111).ok()?,
    ))
}

pub fn poll<T>(
    parser: &mut Parser,
    input: &Receiver<T>,
    elapsed: impl FnMut() -> Duration,
) -> PollResult<T> {
    poll_for(parser, input, elapsed, Duration::MAX)
}

pub fn poll_for<T>(
    parser: &mut Parser,
    input: &Receiver<T>,
    mut elapsed: impl FnMut() -> Duration,
    timeout: Duration,
) -> PollResult<T> {
    let wait = parser.wait(elapsed()).unwrap_or(Duration::MAX).min(timeout);
    // A queued record wins even after Escape expires; it may finish a report.
    let record = receive(input, wait)?;
    let events = if record.is_none() {
        parser.expire(elapsed())
    } else {
        Vec::new()
    };
    Ok((record, events))
}

fn receive<T>(input: &Receiver<T>, wait: Duration) -> Result<Option<T>, String> {
    if wait == Duration::MAX {
        return input
            .recv()
            .map(Some)
            .map_err(|_| "Console input reader stopped".to_owned());
    }
    if wait.is_zero() {
        return match input.try_recv() {
            Ok(value) => Ok(Some(value)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("Console input reader stopped".into()),
        };
    }
    match input.recv_timeout(wait) {
        Ok(value) => Ok(Some(value)),
        Err(RecvTimeoutError::Timeout) => Ok(None),
        Err(RecvTimeoutError::Disconnected) => Err("Console input reader stopped".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_queued_late_paste_record_wins_before_expired_escape() -> Result<(), String> {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b", Duration::ZERO).is_empty());
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(b"[200~draft \xe4\xb8\xad\x1b[201~".to_vec())
            .map_err(|error| error.to_string())?;
        let now = Duration::from_millis(150);
        let (record, events) = poll(&mut parser, &rx, || now)?;
        assert!(events.is_empty(), "queued input must precede cancellation");
        assert_eq!(
            parser.feed(&record.ok_or("queued record missing")?, now),
            vec![Ok(Event::Paste("draft 中".into()))]
        );
        Ok(())
    }

    #[test]
    fn native_records_do_not_renew_escape_and_a_real_escape_dispatches_once() -> Result<(), String>
    {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b", Duration::ZERO).is_empty());
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send("resize").map_err(|error| error.to_string())?;
        let (record, events) = poll(&mut parser, &rx, || Duration::from_millis(90))?;
        assert_eq!(record, Some("resize"));
        assert!(events.is_empty());
        assert_eq!(
            parser.wait(Duration::from_millis(90)),
            Some(Duration::from_millis(10))
        );
        let (record, events) = poll(&mut parser, &rx, || Duration::from_millis(100))?;
        assert!(record.is_none());
        assert_eq!(
            events,
            vec![Ok(Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE
            )))]
        );
        assert_eq!(parser.wait(Duration::from_millis(100)), None);
        drop(tx);
        assert!(poll(&mut parser, &rx, || Duration::from_millis(100)).is_err());
        Ok(())
    }

    #[test]
    fn a_stopped_record_reader_is_an_error_not_an_escape() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b", Duration::ZERO).is_empty());
        let (tx, rx) = mpsc::sync_channel::<u8>(1);
        drop(tx);
        assert!(poll(&mut parser, &rx, || Duration::from_millis(100)).is_err());
        assert_eq!(
            parser.wait(Duration::from_millis(100)),
            Some(Duration::ZERO)
        );
    }
}
