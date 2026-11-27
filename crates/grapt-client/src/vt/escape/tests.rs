use super::*;
use crate::{keymap::Key, vt::PasteRefusal};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

#[test]
fn the_retained_fifty_ms_split_is_one_unicode_multiline_paste() {
    let payload = "\u{1b}[200~INPUT_PASTE_000\nsecond line — ∑ 🙂\u{1b}[201~";
    for split in 1..payload.len() {
        let mut parser = VtParser::new();
        let mut events = parser.feed_at(&payload.as_bytes()[..split], ms(10));
        assert_eq!(parser.flush_expired_escape(ms(30)), None);
        events.extend(parser.feed_at(&payload.as_bytes()[split..], ms(60)));
        assert_eq!(
            events,
            vec![VtEvent::Paste("INPUT_PASTE_000\nsecond line — ∑ 🙂".into())]
        );
        assert_eq!(parser.escape_wait(ms(60)), None);
    }
}

#[test]
fn zero_short_and_repeated_ui_waits_do_not_flush_or_extend_escape() {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(5)).is_empty());
    for now in [5, 6, 25, 55, 104] {
        assert!(parser.feed_at(b"", ms(now)).is_empty());
        assert_eq!(parser.flush_expired_escape(ms(now)), None);
        assert_eq!(parser.escape_wait(ms(now)), Some(ms(105 - now)));
    }
    assert_eq!(
        parser.flush_expired_escape(ms(105)),
        Some(VtEvent::Key(Key::Escape))
    );
    assert_eq!(parser.flush_expired_escape(ms(106)), None);
    assert_eq!(parser.escape_wait(ms(106)), None);
    assert_eq!(
        parser.feed_at(b"q", ms(107)),
        vec![VtEvent::Key(Key::Char('q'))]
    );
}

#[test]
fn queued_continuations_precede_expiry_and_new_escapes_get_their_own_deadline() {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(0)).is_empty());
    assert_eq!(
        parser.feed_at(b"[D", ms(150)),
        vec![VtEvent::Key(Key::Left)]
    );
    assert_eq!(parser.flush_expired_escape(ms(150)), None);
    assert!(parser.feed_at(b"\x1b", ms(200)).is_empty());
    assert_eq!(
        parser.feed_at(b"q\x1b", ms(250)),
        vec![VtEvent::Key(Key::Escape), VtEvent::Key(Key::Char('q'))]
    );
    assert_eq!(parser.escape_wait(ms(299)), Some(ms(51)));
    assert_eq!(
        parser.flush_expired_escape(ms(350)),
        Some(VtEvent::Key(Key::Escape))
    );
}

#[test]
fn partial_text_and_pastes_do_not_schedule_idle_timer_wakes_and_refusal_stays_bounded() {
    let mut parser = VtParser::with_limit(4);
    assert!(parser.feed_at(&[0xe2], ms(0)).is_empty());
    assert_eq!(parser.escape_wait(ms(500)), None);
    assert_eq!(
        parser.feed_at(&[0x88, 0x91], ms(600)),
        vec![VtEvent::Key(Key::Char('∑'))]
    );
    assert!(parser.feed_at(b"\x1b[200~abcde", ms(700)).is_empty());
    assert_eq!(parser.escape_wait(ms(800)), None);
    assert_eq!(parser.flush_expired_escape(ms(5000)), None);
    assert_eq!(
        parser.feed_at(b"\x1b[201~z", ms(6000)),
        vec![
            VtEvent::PasteRefused(PasteRefusal::TooLarge { bytes: 5, limit: 4 }),
            VtEvent::Key(Key::Char('z'))
        ]
    );
}

#[test]
fn a_reader_chunk_queued_at_expiry_keeps_the_complete_write_framed() -> Result<(), String> {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(0)).is_empty());
    let elapsed = std::cell::Cell::new(ms(0));
    // The native counterexample sends this entire frame in one write. Read
    // boundaries may split it, and the producer can publish the remainder
    // after the timed receive expires but before Escape is dispatched.
    let mut reads = std::collections::VecDeque::from([
        (ms(100), None),
        (Duration::ZERO, Some(b"[200~QUIT_DRAFT\x1b[201~".to_vec())),
    ]);
    let events = parser.poll_input(
        ms(1000),
        || elapsed.get(),
        |wait| {
            let (expected, chunk) = reads.pop_front().ok_or("unexpected reader poll")?;
            assert_eq!(wait, expected);
            elapsed.set(ms(150));
            Ok::<_, String>(chunk)
        },
    )?;
    assert_eq!(events, vec![VtEvent::Paste("QUIT_DRAFT".into())]);
    assert!(reads.is_empty());
    assert_eq!(parser.escape_wait(ms(150)), None);
    Ok(())
}

#[test]
fn expired_escape_checks_once_then_idle_reads_keep_the_callers_wait() -> Result<(), String> {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(0)).is_empty());
    let elapsed = std::cell::Cell::new(ms(0));
    let mut waits = Vec::new();
    assert_eq!(
        parser.poll_input(
            ms(1000),
            || elapsed.get(),
            |wait| {
                waits.push(wait);
                elapsed.set(ms(100));
                Ok::<_, String>(None)
            }
        )?,
        vec![VtEvent::Key(Key::Escape)]
    );
    assert_eq!(waits, vec![ms(100), Duration::ZERO]);
    waits.clear();
    assert!(parser
        .poll_input(
            ms(1000),
            || ms(200),
            |wait| {
                waits.push(wait);
                Ok::<_, String>(None)
            }
        )?
        .is_empty());
    assert_eq!(waits, vec![ms(1000)]);
    Ok(())
}

#[test]
fn short_ui_waits_preserve_the_deadline_and_already_queued_bytes_win() -> Result<(), String> {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(0)).is_empty());
    let elapsed = std::cell::Cell::new(ms(0));
    assert!(parser
        .poll_input(
            ms(50),
            || elapsed.get(),
            |wait| {
                assert_eq!(wait, ms(50));
                elapsed.set(ms(50));
                Ok::<_, String>(None)
            }
        )?
        .is_empty());
    assert_eq!(parser.escape_wait(ms(50)), Some(ms(50)));
    assert_eq!(
        parser.poll_input(
            ms(1000),
            || ms(150),
            |wait| {
                assert_eq!(wait, Duration::ZERO);
                Ok::<_, String>(Some("[200~line one\n∑ 🙂\x1b[201~".as_bytes().to_vec()))
            }
        )?,
        vec![VtEvent::Paste("line one\n∑ 🙂".into())]
    );
    assert_eq!(parser.escape_wait(ms(150)), None);
    Ok(())
}

#[test]
fn a_failed_expiry_recheck_reports_the_reader_error_without_dispatching_escape() {
    let mut parser = VtParser::new();
    assert!(parser.feed_at(b"\x1b", ms(0)).is_empty());
    let elapsed = std::cell::Cell::new(ms(0));
    let result = parser.poll_input(
        ms(1000),
        || elapsed.get(),
        |wait| {
            if wait == Duration::ZERO {
                return Err("reader disconnected");
            }
            assert_eq!(wait, ms(100));
            elapsed.set(ms(100));
            Ok(None)
        },
    );
    assert_eq!(result, Err("reader disconnected"));
    assert_eq!(parser.escape_wait(ms(100)), Some(Duration::ZERO));
}
