//! ADDED IN THIS VENDORED COPY: evidence that the budget actually bounds the
//! evaluator, on patterns that are VALID JavaScript and would otherwise run
//! away.
//!
//! Each case is a pattern the frozen oracle compiles happily. The point is not
//! that they are exotic — it is that a caller may legitimately supply one, and
//! before this patch the request path had no bound on what happened next.

#![cfg(all(test, feature = "utf16"))]

use crate::budget::BudgetExhausted;
use crate::Regex;

/// Compile the way the JavaScript wire does: pattern as UTF-16 code units, no
/// flags.
fn js(pattern: &str) -> Regex {
    Regex::from_unicode(pattern.encode_utf16().map(u32::from), crate::Flags::default())
        .expect("pattern is valid JavaScript")
}

fn ucs2(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// The classic catastrophic case, with MEASURED costs rather than guessed ones.
///
/// `^(a+)+$` is cheap when it MATCHES — 40 `a`s cost 58 units, because the
/// greedy first attempt succeeds. It is the FAILING input that explodes: 18 `a`s
/// followed by a character that cannot match costs 3,211,319 units, and 22 `a`s
/// exceeds fifty million. That asymmetry is why "it matched quickly once" is no
/// evidence of a bound, and why the budget is charged rather than timed.
#[test]
fn a_catastrophic_pattern_terminates_on_budget_rather_than_running_away() {
    let re = js("^(a+)+$");
    let text = ucs2(&("a".repeat(22) + "!"));
    let outcome = re.find_from_ucs2_budgeted(&text, 0, 1_000_000);
    assert!(
        matches!(outcome, Err(BudgetExhausted)),
        "an exponential match must stop on its budget"
    );
}

/// Nested quantifiers over a longer input: the budget still ends it, and the
/// SAME budget produces the SAME verdict every time. Determinism is the
/// property that makes an exhausted result reportable rather than a coin flip.
#[test]
fn exhaustion_is_deterministic_across_runs() {
    let re = js("^(x+x+)+y$");
    let text = ucs2(&"x".repeat(30));
    for _ in 0..5 {
        assert!(matches!(re.find_from_ucs2_budgeted(&text, 0, 50_000), Err(BudgetExhausted)));
    }
}

/// A scan restart is charged, so a non-matching pattern over a long input
/// cannot retry at every position for free.
#[test]
fn scan_restarts_are_charged() {
    let re = js("zzz");
    let text = ucs2(&"a".repeat(100_000));
    assert!(matches!(re.find_from_ucs2_budgeted(&text, 0, 1_000), Err(BudgetExhausted)),
        "restarting at every position is work and must be charged"
    );
}

/// Lookaround recursion is charged: it re-enters the instruction loop on the
/// same attempter, so its work lands on the same allowance.
#[test]
fn lookaround_recursion_is_charged() {
    let re = js("^(?=(a+)+$)a*!$");
    let text = ucs2(&("a".repeat(30) + "!"));
    assert!(matches!(re.find_from_ucs2_budgeted(&text, 0, 50_000), Err(BudgetExhausted)));
}

/// A backreference comparison is charged through the instruction loop.
///
/// Measured: `^(a+)\\1+$` against 60 `a`s and a `b` reaches its verdict in 1,435
/// units. Starving it below that must exhaust; funding it above must produce the
/// verdict. Both halves are asserted, because only the pair shows the charge is
/// real rather than the pattern being unconditionally refused.
#[test]
fn backreference_work_is_charged() {
    let re = js(r"^(a+)\1+$");
    let text = ucs2(&("a".repeat(60) + "b"));
    assert!(
        matches!(re.find_from_ucs2_budgeted(&text, 0, 100), Err(BudgetExhausted)),
        "below the measured cost it must exhaust"
    );
    assert!(
        re.find_from_ucs2_budgeted(&text, 0, 100_000)
            .expect("above the measured cost it must reach a verdict")
            .is_none(),
        "and that verdict is a genuine no-match"
    );
}

/// WITHIN budget, the observable answer is unchanged. This is the half that
/// matters for compatibility: the patch bounds when the evaluator stops, never
/// what it decides.
#[test]
fn within_budget_the_verdict_is_identical_to_the_unbudgeted_engine() {
    let cases: &[(&str, &str, bool)] = &[
        ("^[0-9]+$", "12345", true),
        ("^[0-9]+$", "12a45", false),
        (r"^(\w)\1$", "aa", true),
        (r"^(\w)\1$", "ab", false),
        ("^(?=.*[0-9]).+$", "a1", true),
        ("^(?=.*[0-9]).+$", "ab", false),
        ("^.$", "😀", false),
        ("^.{2}$", "😀", true),
        (r"^\p{L}+$", "p{L}", true),
        (r"^\w+$", "é", false),
    ];
    for (pattern, value, expected) in cases {
        let re = js(pattern);
        let text = ucs2(value);
        let budgeted = re
            .find_from_ucs2_budgeted(&text, 0, 1_000_000)
            .expect("ordinary work stays well inside the budget");
        let unbudgeted = re.find_from_ucs2(&text, 0).next();
        assert_eq!(
            budgeted.is_some(),
            *expected,
            "pattern {pattern:?} against {value:?}"
        );
        assert_eq!(
            budgeted.is_some(),
            unbudgeted.is_some(),
            "budgeted and unbudgeted must agree within budget"
        );
    }
}

/// Exhaustion must never be reported as a verdict.
///
/// The SAME pattern and the SAME input, starved and then funded: starved it
/// reports exhaustion, funded it reports a genuine no-match. If exhaustion were
/// collapsed into `Ok(None)` these two would be indistinguishable, and a caller
/// would tell the user their answer was rejected when in truth we never
/// finished checking. Measured cost of the funded run: 3,211,319 units.
#[test]
fn exhaustion_is_never_translated_into_no_match() {
    let re = js("^(a+)+$");
    let text = ucs2(&("a".repeat(18) + "!"));
    assert!(
        matches!(re.find_from_ucs2_budgeted(&text, 0, 1_000), Err(BudgetExhausted)),
        "starved: the answer is I STOPPED, not it does not match"
    );
    assert!(
        re.find_from_ucs2_budgeted(&text, 0, 5_000_000)
            .expect("funded: reaches a verdict")
            .is_none(),
        "funded: a genuine no-match, distinguishable from exhaustion"
    );
}
