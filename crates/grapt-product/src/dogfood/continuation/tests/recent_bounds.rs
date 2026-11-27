//! The recent projection past the 256 KiB DTO string bound (boundary
//! review and runtime audit): the
//! conversation, its parts, the discarded head and the recent half are
//! internal text; only the published recent value is a public string, and
//! past the bound it keeps its newest text behind a visible marker.

use super::super::recent::{
    prepare_recent, select_recent, serialize_assistant, ConversationEntry, Part,
};
use super::*;
use grapt_api::u3a::Utf16Text;

fn entry(seq: u64, user: bool, text: &str) -> ConversationEntry {
    ConversationEntry {
        seq,
        user,
        text: text.encode_utf16().collect(),
    }
}

/// The str form of internal units, or None for a lone surrogate.
fn utf16(units: &[u16]) -> Option<String> {
    String::from_utf16(units).ok()
}

/// The review's boundary: the discarded head may exceed the DTO
/// string bound, since only the published recent value is a public string.
/// 131_072 + 2 + 131_071 encoded bytes is one past the bound.
#[test]
fn a_discarded_head_past_the_dto_bound_still_selects() -> Result<(), String> {
    let first = format!(
        "[User]: {}",
        "a".repeat(131_072 - super::super::recent::USER_LABEL.len())
    );
    let second = format!(
        "[Assistant]: {}",
        "b".repeat(131_071 - super::super::recent::ASSISTANT_LABEL.len())
    );
    let entries = vec![
        entry(0, true, &first),
        entry(1, false, &second),
        entry(2, true, "[User]: c"),
    ];
    let (head, recent, public) = select_recent(&entries, 8_000, &Utf16Text::empty())?;
    assert_eq!(
        head.len(),
        131_072 + 2 + 131_071,
        "the head is the whole discard"
    );
    assert_eq!(utf16(&recent).as_deref(), Some("[User]: c"));
    assert_eq!(public.as_str().as_deref(), Some("[User]: c"));
    Ok(())
}

/// The live path: a window whose discarded head is past the DTO string
/// bound, though well inside the 4 MiB context budget, projects its recent
/// value instead of refusing the compaction.
#[test]
fn a_window_whose_head_is_past_the_dto_bound_projects() -> Result<(), String> {
    let fixture = Fixture::new("srp-head-past-dto")?;
    {
        let inbox = lock(fixture.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for seq in 0..13i64 {
            seed.user_session_message(
                &format!("msg_srp_head_{seq}"),
                fixture.session.as_str(),
                &format!("{seq:02}{}", "a".repeat(25_000)),
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 12)
        .map_err(|error| format!("a head past the DTO bound refused: {error}"))?;
    assert_eq!(
        recent.as_str().as_deref(),
        Some(format!("[User]: 12{}", "a".repeat(25_000)).as_str()),
        "the projection publishes the final entry"
    );
    Ok(())
}

/// Audit sites 1, 4 and 5: assistant parts and their
/// serialized entry are internal text. Two legal 131_072-byte text parts,
/// and 1 MiB text and reasoning blocks, serialize instead of refusing.
#[test]
fn assistant_parts_past_the_dto_bound_serialize() {
    let units =
        |fill: &str, count: usize| -> Vec<u16> { fill.repeat(count).encode_utf16().collect() };
    let two = serialize_assistant(&[
        Part::Text(units("a", 131_072)),
        Part::Text(units("b", 131_072)),
    ])
    .unwrap_or_default();
    assert_eq!(
        two.len(),
        2 * (super::super::recent::ASSISTANT_LABEL.len() + 131_072) + 1,
        "two legal parts join one past the bound"
    );
    let blocks = serialize_assistant(&[
        Part::Reasoning(units("r", 1 << 20)),
        Part::Text(units("t", 1 << 20)),
    ])
    .unwrap_or_default();
    assert!(blocks.len() > 2 << 20, "1 MiB blocks serialize whole");
}

/// Audit site 3: authorized synthetic text up to the DTO bound plus its
/// label is one entry past the bound; the live path still projects.
#[test]
fn a_synthetic_entry_past_the_dto_bound_projects() -> Result<(), String> {
    let fixture = Fixture::new("srp-synthetic-past-dto")?;
    {
        let inbox = lock(fixture.inbox()?);
        let seed = inbox
            .store()
            .fixture_rows()
            .map_err(|error| error.to_string())?;
        for (seq, kind, text) in [
            (0i64, "synthetic", "s".repeat(262_144)),
            (1, "user", "latest".to_owned()),
        ] {
            seed.session_message_row(
                &format!("msg_srp_synthetic_{seq}"),
                fixture.session.as_str(),
                kind,
                &text,
                "queue",
                seq,
            )
            .map_err(|error| error.to_string())?;
        }
        seed.commit().map_err(|error| error.to_string())?;
    }
    let inbox = lock(fixture.inbox()?);
    let recent = prepare_recent(&inbox, fixture.artifacts()?, &fixture.session, 1)
        .map_err(|error| format!("a synthetic entry past the DTO bound refused: {error}"))?;
    assert_eq!(recent.as_str().as_deref(), Some("[User]: latest"));
    Ok(())
}

/// Audit site 2 and its rule: the keep budget is soft,
/// so the recent half can outgrow the DTO bound; the published value then
/// keeps its NEWEST text behind the marker and never refuses.
#[test]
fn a_recent_half_past_the_dto_bound_publishes_its_newest_text() -> Result<(), String> {
    let user = format!(
        "[User]: {}",
        "u".repeat(200 - super::super::recent::USER_LABEL.len())
    );
    let tail = format!("{}END", "z".repeat(262_000 - 3));
    let entries = vec![
        entry(0, true, "[User]: old"),
        entry(1, false, "[Assistant]: old"),
        entry(2, true, &user),
        entry(3, false, &tail),
    ];
    let (_, recent, public) = select_recent(&entries, 8_000, &Utf16Text::empty())?;
    assert_eq!(recent.len(), 200 + 2 + 262_000, "the half itself is whole");
    let text = public.as_str().unwrap_or_default();
    assert!(
        text.starts_with(super::super::recent::RECENT_CLIPPED_MARKER),
        "the clip is visible"
    );
    assert!(text.ends_with("END"), "the newest text survives");
    assert!(
        text.len() <= 262_144,
        "the published value fits: {}",
        text.len()
    );
    assert!(
        !text.contains("[User]: uuu"),
        "the oldest retained text was dropped"
    );

    // A surrogate pair is never split at the cut.
    let emoji = "\u{1F600}".repeat(70_000);
    let pairs = vec![entry(0, true, "[User]: old"), entry(1, true, &emoji)];
    let (_, _, public) = select_recent(&pairs, 1, &Utf16Text::empty())?;
    let marker = super::super::recent::RECENT_CLIPPED_MARKER
        .encode_utf16()
        .count();
    let first = public.units().get(marker).copied().unwrap_or_default();
    assert!(
        !(0xDC00..=0xDFFF).contains(&first),
        "the cut split a surrogate pair"
    );
    assert!(public.as_str().is_some(), "no lone surrogate was published");
    Ok(())
}
