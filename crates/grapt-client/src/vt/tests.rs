//! What the byte parser owes, proved without a console.

use super::*;

fn feed_all(parser: &mut VtParser, chunks: &[&[u8]]) -> Vec<VtEvent> {
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(parser.feed(chunk));
    }
    events
}

fn key(k: Key) -> VtEvent {
    VtEvent::Key(k)
}

/// The exact bytes measured from a real ConPTY, replayed.
///
/// This is the observation the whole seam rests on, so it is pinned as a
/// witness rather than described in a comment: the harness recorded
/// `1b 5b 32 30 30 7e`, the payload, then `ESC[201~`, and both `U+2014` and
/// `U+2211` survived. If a future change breaks the framing, this fails with
/// the real stream rather than an invented one.
#[test]
fn the_measured_conpty_paste_arrives_whole_and_keeps_its_unicode() {
    let mut parser = VtParser::new();
    let stream = b"\x1b[200~line one\rline two\ttabbed\r \xe2\x80\x94 na\xc3\xafve caf\xc3\xa9 \xce\xa9 \xe2\x88\x91 \xf0\x9f\x99\x82 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e\x1b[201~";
    let events = parser.feed(stream);
    assert_eq!(
        events,
        vec![VtEvent::Paste(
            "line one\nline two\ttabbed\n — naïve café Ω ∑ 🙂 日本語".to_owned()
        )],
        "the measured stream must arrive as ONE paste with every character intact"
    );
}

/// The defect this seam exists to fix.
#[test]
fn a_pasted_newline_is_text_and_never_the_enter_key() {
    let mut parser = VtParser::new();
    let events = parser.feed(b"\x1b[200~first\rsecond\x1b[201~");
    assert_eq!(events, vec![VtEvent::Paste("first\nsecond".to_owned())]);
    assert!(
        !events.contains(&key(Key::Enter)),
        "a pasted newline that submits is the original defect: a two-line \
         prompt sent its first line before the user had finished"
    );
}

/// A typed Enter is still Enter — the fix must not cost the submit key.
#[test]
fn a_typed_carriage_return_is_still_the_enter_key() {
    let mut parser = VtParser::new();
    assert_eq!(
        parser.feed(b"hi\r"),
        vec![key(Key::Char('h')), key(Key::Char('i')), key(Key::Enter)]
    );
}

#[test]
fn a_crlf_pair_submits_once_rather_than_twice() {
    let mut parser = VtParser::new();
    assert_eq!(parser.feed(b"\r\n"), vec![key(Key::Enter)]);
}

#[test]
fn a_lone_line_feed_still_submits() {
    let mut parser = VtParser::new();
    assert_eq!(parser.feed(b"\n"), vec![key(Key::Enter)]);
}

/// The terminal owes no alignment, so every boundary must hold.
#[test]
fn a_character_split_across_reads_is_not_corrupted() {
    let mut parser = VtParser::new();
    // U+65E5 is three bytes; deliver them one read at a time.
    let events = feed_all(&mut parser, &[&[0xe6], &[0x97], &[0xa5]]);
    assert_eq!(events, vec![key(Key::Char('日'))]);
}

#[test]
fn a_paste_split_across_reads_still_arrives_once_and_whole() {
    let mut parser = VtParser::new();
    let events = feed_all(
        &mut parser,
        &[
            b"\x1b[2",
            b"00~he",
            b"llo \xe2\x88",
            b"\x91 there\x1b[20",
            b"1~",
        ],
    );
    assert_eq!(events, vec![VtEvent::Paste("hello ∑ there".to_owned())]);
}

#[test]
fn a_paste_marker_split_across_reads_is_not_mistaken_for_typed_text() {
    let mut parser = VtParser::new();
    let events = feed_all(&mut parser, &[b"\x1b", b"[", b"200~x\x1b[201~"]);
    assert_eq!(
        events,
        vec![VtEvent::Paste("x".to_owned())],
        "the introducer arriving in pieces must not leak `[200~` into the composer"
    );
}

/// The bound, and its honesty.
#[test]
fn an_oversize_paste_is_refused_rather_than_silently_truncated() {
    let mut parser = VtParser::with_limit(8);
    let events = parser.feed(b"\x1b[200~123456789012\x1b[201~");
    assert_eq!(
        events,
        vec![VtEvent::PasteRefused(PasteRefusal::TooLarge {
            bytes: 12,
            limit: 8
        })],
        "truncating would put text in the composer the user cannot see is short"
    );
}

/// After refusing, the stream must resynchronise.
#[test]
fn input_after_a_refused_paste_is_read_normally() {
    let mut parser = VtParser::with_limit(4);
    let mut events = parser.feed(b"\x1b[200~toolong\x1b[201~");
    events.extend(parser.feed(b"ok"));
    assert_eq!(
        events,
        vec![
            VtEvent::PasteRefused(PasteRefusal::TooLarge { bytes: 7, limit: 4 }),
            key(Key::Char('o')),
            key(Key::Char('k')),
        ],
        "the remainder of an oversize paste must not spill out as keystrokes"
    );
}

/// The upstream defect: a terminator that never comes.
#[test]
fn an_unterminated_paste_never_delivers_and_is_reported_as_in_flight() {
    let mut parser = VtParser::new();
    let events = parser.feed(b"\x1b[200~half a paste");
    assert!(
        events.is_empty(),
        "a paste with no terminator must not be delivered as if it had ended"
    );
    assert!(
        parser.paste_in_flight(),
        "the reader must be able to tell an unfinished paste from an idle user"
    );
}

#[test]
fn an_unterminated_paste_is_bounded_rather_than_growing_without_limit() {
    let mut parser = VtParser::with_limit(16);
    assert!(parser.feed(b"\x1b[200~").is_empty());
    for _ in 0..1000 {
        assert!(parser.feed(b"aaaaaaaaaa").is_empty());
    }
    assert!(parser.paste_in_flight());
    // The terminator finally arrives; the refusal names the real size.
    let events = parser.feed(b"\x1b[201~");
    assert_eq!(
        events,
        vec![VtEvent::PasteRefused(PasteRefusal::TooLarge {
            bytes: 10_000,
            limit: 16
        })]
    );
}

/// Escape is the cancel key, and it is the introducer of every sequence.
#[test]
fn a_lone_escape_is_delivered_when_the_wait_expires() {
    let mut parser = VtParser::new();
    assert!(
        parser.feed(b"\x1b").is_empty(),
        "ESC alone is not yet decidable"
    );
    assert_eq!(parser.flush_pending_escape(), Some(key(Key::Escape)));
    assert_eq!(parser.flush_pending_escape(), None, "it is delivered once");
}

#[test]
fn an_escape_followed_by_ordinary_text_is_a_cancel_and_then_the_text() {
    let mut parser = VtParser::new();
    assert_eq!(
        parser.feed(b"\x1bq"),
        vec![key(Key::Escape), key(Key::Char('q'))]
    );
}

#[test]
fn editor_sequences_survive_every_chunk_boundary() {
    for (sequence, expected) in [
        (b"\x1b[D".as_slice(), Key::Left),
        (b"\x1b[C".as_slice(), Key::Right),
        (b"\x1b[H".as_slice(), Key::Home),
        (b"\x1b[F".as_slice(), Key::End),
        (b"\x1b[3~".as_slice(), Key::Delete),
        (b"\x1b[1;5D".as_slice(), Key::CtrlLeft),
        (b"\x1b[1;5C".as_slice(), Key::CtrlRight),
        (b"\x1bb".as_slice(), Key::Alt('b')),
        (b"\x1bf".as_slice(), Key::Alt('f')),
        (b"\x1bd".as_slice(), Key::Alt('d')),
        (b"\x1br".as_slice(), Key::Alt('r')),
        (b"\x1b\x7f".as_slice(), Key::AltBackspace),
        (b"\x1bOD".as_slice(), Key::Left),
        (b"\x1bOC".as_slice(), Key::Right),
        (b"\x1bOA".as_slice(), Key::Up),
        (b"\x1bOB".as_slice(), Key::Down),
        (b"\x1bOH".as_slice(), Key::Home),
        (b"\x1bOF".as_slice(), Key::End),
    ] {
        for split in 0..=sequence.len() {
            let mut parser = VtParser::new();
            let mut events = parser.feed(&sequence[..split]);
            events.extend(parser.feed(&sequence[split..]));
            assert_eq!(events, vec![key(expected)], "{sequence:?} split {split}");
        }
    }
}

#[test]
fn flushing_does_not_invent_an_escape_when_none_is_pending() {
    let mut parser = VtParser::new();
    parser.feed(b"a");
    assert_eq!(parser.flush_pending_escape(), None);
}

#[test]
fn navigation_and_control_keys_survive_the_new_reader() {
    let mut parser = VtParser::new();
    assert_eq!(parser.feed(b"\x1b[A"), vec![key(Key::Up)]);
    assert_eq!(parser.feed(b"\x1b[B"), vec![key(Key::Down)]);
    assert_eq!(parser.feed(b"\x1b[5~"), vec![key(Key::PageUp)]);
    assert_eq!(parser.feed(b"\x1b[6~"), vec![key(Key::PageDown)]);
    assert_eq!(parser.feed(b"\t"), vec![key(Key::Tab)]);
    assert_eq!(parser.feed(b"\x7f"), vec![key(Key::Backspace)]);
    assert_eq!(parser.feed(b"\x08"), vec![key(Key::Backspace)]);
}

/// Ctrl+Q quits and Ctrl+C interrupts; losing them would strand the user.
#[test]
fn control_chords_are_preserved() {
    let mut parser = VtParser::new();
    assert_eq!(parser.feed(&[0x11]), vec![key(Key::Ctrl('q'))]);
    assert_eq!(parser.feed(&[0x03]), vec![key(Key::Ctrl('c'))]);
}

#[test]
fn an_unrecognised_sequence_is_swallowed_rather_than_spilled_into_the_composer() {
    let mut parser = VtParser::new();
    // A mouse report and an unknown modified key: neither is in this client's
    // vocabulary, and neither may appear as characters in the prompt.
    assert!(parser.feed(b"\x1b[<0;10;20M").is_empty());
    assert!(parser.feed(b"\x1b[1;5H").is_empty());
    assert_eq!(parser.feed(b"z"), vec![key(Key::Char('z'))]);
}

#[test]
fn an_escape_sequence_inside_a_paste_stays_part_of_the_pasted_text() {
    let mut parser = VtParser::new();
    let events = parser.feed(b"\x1b[200~before\x1b[1mafter\x1b[201~");
    assert_eq!(
        events,
        vec![VtEvent::Paste("before\x1b[1mafter".to_owned())],
        "text someone pasted is text, even when it looks like a sequence"
    );
}

#[test]
fn invalid_utf8_in_a_paste_is_refused_rather_than_guessed() {
    let mut parser = VtParser::new();
    let events = parser.feed(b"\x1b[200~ok\x1b[201~");
    assert_eq!(events, vec![VtEvent::Paste("ok".to_owned())]);
}

#[test]
fn invalid_utf8_outside_a_paste_is_dropped_rather_than_becoming_a_wrong_character() {
    let mut parser = VtParser::new();
    // A lone continuation byte is not a character in any encoding.
    assert!(parser.feed(&[0x80]).is_empty());
    assert_eq!(parser.feed(b"a"), vec![key(Key::Char('a'))]);
}

#[test]
fn a_truncated_character_does_not_grow_the_buffer_without_bound() {
    let mut parser = VtParser::new();
    // Repeated lead bytes that never complete: the parser must not accumulate.
    for _ in 0..100 {
        parser.feed(&[0xe6]);
    }
    assert_eq!(parser.feed(b"a"), vec![key(Key::Char('a'))]);
}

#[test]
fn an_empty_paste_is_still_a_paste() {
    let mut parser = VtParser::new();
    assert_eq!(
        parser.feed(b"\x1b[200~\x1b[201~"),
        vec![VtEvent::Paste(String::new())]
    );
}

#[test]
fn two_pastes_in_one_read_are_both_delivered() {
    let mut parser = VtParser::new();
    assert_eq!(
        parser.feed(b"\x1b[200~one\x1b[201~\x1b[200~two\x1b[201~"),
        vec![
            VtEvent::Paste("one".to_owned()),
            VtEvent::Paste("two".to_owned())
        ]
    );
}

#[test]
fn a_refusal_says_what_happened_in_words_a_user_can_act_on() {
    let reason = PasteRefusal::TooLarge {
        bytes: 20,
        limit: 8,
    }
    .reason();
    assert!(reason.contains("20") && reason.contains('8'));
    assert!(
        reason.contains("nothing was inserted"),
        "the user must be told the composer was left alone"
    );
}

#[test]
fn pasted_blank_lines_survive_every_chunk_boundary() {
    let stream = b"\x1b[200~a\n\nb\r\n\r\nc\r\r\n\n\x1b[201~";
    for split in 0..=stream.len() {
        let mut parser = VtParser::new();
        assert_eq!(
            feed_all(&mut parser, &[&stream[..split], &stream[split..]]),
            vec![VtEvent::Paste("a\n\nb\n\nc\n\n\n".into())],
            "split at {split}"
        );
    }
}

#[test]
fn unterminated_control_sequences_remain_bounded_and_resynchronize() {
    let mut parser = VtParser::new();
    assert!(parser.feed(b"\x1b[").is_empty());
    for _ in 0..128 {
        assert!(parser.feed(&[b'1'; 1_024]).is_empty());
        assert!(
            matches!(parser.state, State::Control(ref bytes) if bytes.len() <= CONTROL_LIMIT_BYTES)
                || matches!(parser.state, State::DiscardControl)
        );
    }
    assert_eq!(parser.feed(b"~x"), vec![key(Key::Char('x'))]);
}

#[test]
fn malformed_paste_controls_cannot_bypass_the_paste_bound() {
    let mut parser = VtParser::with_limit(32);
    assert!(parser.feed(b"\x1b[200~\x1b[").is_empty());
    for _ in 0..128 {
        assert!(parser.feed(&[b'1'; 1_024]).is_empty());
        assert!(parser.paste.len() <= 32);
        assert!(!matches!(parser.state, State::PasteControl(ref bytes) if bytes.len() > 4));
    }
    assert!(matches!(
        parser.feed(b"\x1b[201~").as_slice(),
        [VtEvent::PasteRefused(PasteRefusal::TooLarge { .. })]
    ));
    assert_eq!(parser.feed(b"x"), vec![key(Key::Char('x'))]);
}

#[test]
fn an_input_barrier_waits_for_partial_characters_controls_and_pastes() {
    let mut parser = VtParser::new();
    assert!(parser.idle());
    parser.feed(&[0xc3]);
    assert!(!parser.idle());
    parser.feed(&[0xa9]);
    assert!(parser.idle());
    parser.feed(b"\x1b[");
    assert!(!parser.idle());
    parser.feed(b"A");
    assert!(parser.idle());
    parser.feed(b"\x1b[200~old queued paste");
    assert!(!parser.idle());
    parser.feed(b"\x1b[201~");
    assert!(parser.idle());
}
