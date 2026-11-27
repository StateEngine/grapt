//! The retained facts keep the task a Session began with (the
//! Luna C04 control-r1: summary 3 said the requirements were never
//! supplied). Newest-first quoting let each 24.7 KB archive prompt fill the
//! whole bound, so the first input fell out of the facts.

use super::*;
use crate::dogfood::continuation::retained_facts::retained_facts;

/// C04's archive-review prompt length.
const ARCHIVE_BYTES: usize = 24_687;

/// The facts the last of `later` sees, in a Session that began with `first`.
fn facts_after(label: &str, first: &str, later: &[String]) -> Result<String, String> {
    let fixture = Fixture::new(label)?;
    promote_test_input(fixture.inbox()?, &fixture.session, &fixture.input, first)?;
    let mut current = fixture.input.clone();
    for (index, text) in later.iter().enumerate() {
        current = format!("msg_facts_{index:02}")
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(fixture.inbox()?, &fixture.session, &current, text)?;
    }
    let inbox = lock(fixture.inbox()?);
    retained_facts(
        &inbox,
        fixture.artifacts()?,
        &fixture.session,
        &current,
        grapt_types::EventSeq::new(1_000_000),
    )?
    .ok_or_else(|| "no facts were retained".to_owned())
}

#[test]
fn the_first_input_survives_inputs_that_fill_the_bound() -> Result<(), String> {
    let archive = |name: &str| format!("ARCHIVE {name}: {}", "a".repeat(ARCHIVE_BYTES));
    let facts = facts_after(
        "facts-bound-first",
        "use the tool",
        &[
            archive("one"),
            archive("two"),
            archive("three"),
            "now finish".to_owned(),
        ],
    )?;
    // The original task, verbatim, first.
    let first = facts
        .lines()
        .position(|line| line == "- \"use the tool\"")
        .ok_or_else(|| format!("the original task was lost: {facts:.400}"))?;
    // The newest earlier input still follows it, cut to the bound and marked.
    let newest = facts
        .lines()
        .position(|line| line.starts_with("- \"ARCHIVE three"))
        .ok_or("the newest earlier input was lost")?;
    assert!(first < newest);
    assert!(facts.contains(" bytes)\n"), "the cut input is not marked");
    // What lies between them is named, not silently dropped.
    assert!(
        facts.contains("- (2 later inputs omitted by the retained bound)\n"),
        "{facts:.600}"
    );
    assert!(facts.contains("Earlier user inputs (2 of 4, oldest first):"));
    // The current input is never an earlier input.
    assert!(!facts.contains("now finish"));
    Ok(())
}

#[test]
fn inputs_within_the_bound_are_all_kept_in_order() -> Result<(), String> {
    let facts = facts_after(
        "facts-bound-small",
        "use the tool",
        &["second".to_owned(), "third".to_owned(), "now".to_owned()],
    )?;
    assert!(
        facts.contains(
            "Earlier user inputs (3 of 3, oldest first):\n- \"use the tool\"\n- \"second\"\n- \"third\"\n"
        ),
        "{facts}"
    );
    assert!(!facts.contains("omitted by the retained bound"));
    Ok(())
}

#[test]
fn a_long_first_input_keeps_only_its_share() -> Result<(), String> {
    let facts = facts_after(
        "facts-bound-long-first",
        &format!("TASK {}", "t".repeat(20_000)),
        &["later".to_owned(), "now".to_owned()],
    )?;
    let first = facts
        .lines()
        .find(|line| line.starts_with("- \"TASK "))
        .ok_or("the long first input was lost")?;
    assert!(first.len() <= 8 * 1024 + 2, "{}", first.len());
    assert!(
        first.ends_with(" of 20005 bytes)"),
        "{:.80}",
        &first[first.len() - 40..]
    );
    assert!(facts.contains("- \"later\"\n"));
    Ok(())
}

/// Review FB-H1: escapes expand a task's JSON form, and the
/// old shrink overshot to nothing. The task keeps a fitting prefix and its
/// marker however escape-heavy it is.
#[test]
fn an_escape_heavy_first_input_keeps_a_prefix_and_its_marker() -> Result<(), String> {
    let task = format!("TASK fix parser:\n{}", "\n\t\"".repeat(10_000));
    let archive = |name: &str| format!("ARCHIVE {name}: {}", "a".repeat(ARCHIVE_BYTES));
    let facts = facts_after(
        "facts-bound-escapes",
        &task,
        &[archive("one"), archive("two"), "now".to_owned()],
    )?;
    let first = facts
        .lines()
        .find(|line| line.starts_with("- \"TASK fix parser:\\n"))
        .ok_or_else(|| format!("the escape-heavy task was lost: {facts:.300}"))?;
    assert!(first.len() <= 8 * 1024 + 2, "{}", first.len());
    assert!(
        first.ends_with(&format!(" of {} bytes)", task.len())),
        "{}",
        &first[first.len() - 60..]
    );
    assert!(facts.contains("Earlier user inputs (2 of 3, oldest first):"));
    Ok(())
}

/// Quotes, backslashes and control characters: the kept prefix is valid
/// JSON, decodes to exactly the input's first bytes, and fills its share.
#[test]
fn a_cut_first_input_decodes_to_its_exact_prefix() -> Result<(), String> {
    let task = format!("TASK \\ \"q\" \u{1}{}", "\\\"\u{1}\u{7f}é".repeat(4_000));
    let facts = facts_after("facts-bound-prefix", &task, &["now".to_owned()])?;
    let first = facts
        .lines()
        .find(|line| line.starts_with("- \"TASK "))
        .ok_or("the task was lost")?;
    let (literal, marker) = first
        .strip_prefix("- ")
        .and_then(|line| line.rsplit_once(" (first "))
        .ok_or("the cut task has no marker")?;
    let end: usize = marker
        .split_once(" of ")
        .and_then(|(end, _)| end.parse().ok())
        .ok_or("the marker names no prefix length")?;
    let decoded: String = serde_json::from_str(literal).map_err(|error| error.to_string())?;
    assert_eq!(decoded, task[..end]);
    // Within its share, and not short of it by more than one escape.
    assert!(first.len() <= 8 * 1024 + 2, "{}", first.len());
    assert!(first.len() + 8 >= 8 * 1024, "{}", first.len());
    Ok(())
}

/// The escaped length is serde_json's, character by character.
#[test]
fn the_escaped_length_is_what_json_writes() {
    use crate::dogfood::continuation::retained_facts::escaped_len;
    for character in (0u32..0x80)
        .filter_map(char::from_u32)
        .chain(['\u{7f}', 'é', '€', '😀', '\u{2028}'])
    {
        let written =
            serde_json::to_string(&character.to_string()).map_or(0, |json| json.len() - 2);
        assert_eq!(escaped_len(character), written, "{character:?}");
    }
}
