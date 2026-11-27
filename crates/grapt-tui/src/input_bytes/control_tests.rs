use super::*;

fn text(events: Vec<Result<Event, String>>) -> String {
    events
        .into_iter()
        .filter_map(|event| match event {
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Char(ch),
                ..
            })) => Some(ch),
            _ => None,
        })
        .collect()
}

#[test]
fn oversized_mouse_control_discards_its_suffix_until_the_final_byte() {
    let mut parser = Parser::new();
    let mut report = b"\x1b[<".to_vec();
    report.extend(std::iter::repeat_n(b'9', 512));
    report.extend_from_slice(b";30;10M[123;45");
    assert_eq!(text(parser.feed(&report, Duration::ZERO)), "[123;45");
    assert!(parser.idle());
}

#[test]
fn partial_mouse_controls_expire_and_quarantine_only_the_report_tail() {
    for report in [b"\x1b[<64;30;10M".as_slice(), b"\x1b[M`$*"] {
        for split in 2..report.len() {
            let mut parser = Parser::new();
            assert!(parser.feed(&report[..split], Duration::ZERO).is_empty());
            assert_eq!(
                parser.wait(Duration::from_millis(99)),
                Some(Duration::from_millis(1))
            );
            let expired = parser.expire(Duration::from_millis(100));
            assert!(expired.iter().any(Result::is_err));
            assert!(
                parser.idle(),
                "a dropped control must not hold startup forever"
            );
            assert_eq!(parser.wait(Duration::from_millis(100)), None);
            assert!(text(parser.feed(&report[split..], Duration::from_millis(101))).is_empty());
            assert_eq!(
                text(parser.feed(b"[123;45", Duration::from_millis(102))),
                "[123;45"
            );
        }
    }
}

#[test]
fn mouse_reports_survive_every_split_and_128_record_packet_boundary() {
    let report = b"\x1b[<64;30;10M";
    for split in 0..=report.len() {
        let mut parser = Parser::new();
        let mut events = parser.feed(&report[..split], Duration::ZERO);
        events.extend(parser.feed(&report[split..], Duration::from_millis(50)));
        assert_eq!(events.len(), 1);
        assert!(matches!(events.first(), Some(Ok(Event::Mouse(_)))));
        assert!(parser.idle());
    }
    for offset in 0..128 {
        let mut parser = Parser::new();
        let mut burst = vec![b'q'; offset];
        for _ in 0..600 {
            burst.extend_from_slice(report);
        }
        burst.extend_from_slice(b"[123;45qk");
        let events: Vec<_> = burst
            .chunks(128)
            .flat_map(|chunk| parser.feed(chunk, Duration::ZERO))
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Ok(Event::Mouse(_))))
                .count(),
            600
        );
        assert_eq!(text(events), format!("{}[123;45qk", "q".repeat(offset)));
        assert!(parser.idle());
    }
}

#[test]
fn unknown_mouse_reports_are_consumed_but_pasted_reports_are_literal_text() {
    let mut parser = Parser::new();
    assert!(text(parser.feed(b"\x1b[<99;30;10M\x1b[96;30;10M", Duration::ZERO)).is_empty());
    assert_eq!(
        parser.feed(b"\x1b[200~[123 \x1b[<64;30;10M\x1b[201~", Duration::ZERO),
        vec![Ok(Event::Paste("[123 \x1b[<64;30;10M".into()))]
    );
    assert_eq!(text(parser.feed(b"[123;45qk", Duration::ZERO)), "[123;45qk");
    // Missing framing cannot be guessed: these are also valid typed literals.
    assert_eq!(
        text(parser.feed(b"4;30;10M[<64;30;10M", Duration::ZERO)),
        "4;30;10M[<64;30;10M"
    );
}

#[test]
fn a_new_escape_resynchronizes_a_refused_report_without_a_periodic_idle_timer() {
    let mut parser = Parser::new();
    assert!(parser.feed(b"\x1b[<64;", Duration::ZERO).is_empty());
    assert!(!parser.expire(Duration::from_millis(100)).is_empty());
    assert!(parser.idle());
    assert_eq!(parser.wait(Duration::from_secs(3600)), None);
    assert!(matches!(
        parser
            .feed(b"\x1b[<65;30;10M", Duration::from_secs(3600))
            .first(),
        Some(Ok(Event::Mouse(_)))
    ));
    assert_eq!(
        text(parser.feed(b"qk[123", Duration::from_secs(3600))),
        "qk[123"
    );
}
