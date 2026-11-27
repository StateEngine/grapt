//! Architecture gate (C3-4): no Rust crate reimplements the generated-client
//! emitter's textual rewrites.
//!
//! The ratified source is the C3 decision memo,
//! `fixtures/rust-reconstruction/compat/GENERATOR_CONTRACT_DECISION_C3_2026-08-08.md`
//! in the frozen oracle tree — **not** the reconstruction master, whose §3.3
//! is a different subject entirely. C3 §5 item 4: *"Rust does not reimplement
//! the quirks … Attempting to reproduce seed-plus-suffix numbering, textual
//! `unknown`→`any` widening, or `(string & {})` autocomplete hacks in Rust would
//! be porting accidents into a rebuild whose stated purpose is to stop doing
//! that."* Acceptance row C3-4 is the executable half of that, and this file is
//! C3-4 discharged.
//!
//! The ruling of 2026-08-14 fixes the scope: **all five textual passes
//! described in §3.3**, and *not* §3.1's seed-plus-suffix numbering, which is a
//! naming algorithm rather than a textual rewrite and belongs to no gate here.
//!
//! # The five passes, and what each leaves behind in Rust
//!
//! Read at TS canonical `cf124ce4`, blob `packages/httpapi-codegen/src/index.ts`
//! (1,776 lines). The first three are one chained expression inside `render`,
//! which lives in `structuralTypes` (`index.ts:986`) and is reached exactly once,
//! from `index.ts:838`:
//!
//! ```text
//! .replaceAll(/ & Brand\.Brand<"[^"]+">/g, "")        // 1014  brand erasure
//! .replaceAll("Schema.Json", "JsonValue")             // 1015  alias
//! .replaceAll(/(?<!["'])\bunknown\b(?!["'])/g, "any") // 1016  widening
//! ```
//!
//! and two more in the same section:
//!
//! ```text
//! preserveStringSuggestions (1064): "a"|"b"|string  ->  "a"|"b"|(string & {})
//! mutableType (852): "ReadonlyArray<" -> "Array<", and /\breadonly\s+/g -> ""
//! ```
//!
//! # Why the detector is in two classes
//!
//! Four of the five leave a token behind that has **no lawful reason to exist in
//! Rust source**: `Brand.Brand<`, `Schema.Json`, `JsonValue`, `(string & {})`,
//! `ReadonlyArray<`. Those are matched directly (class A). Measured at base
//! `2e93c9d4`: zero occurrences across all 397 Rust files.
//!
//! The widening is different and this is the whole difficulty of the gate.
//! `unknown` and `any` are ordinary English words and ordinary Rust identifiers —
//! at base this workspace has **162** bare `unknown` and **433** bare `any`
//! (`std::any`, `Iterator::any`). A gate keying on either word alone reds on
//! shipped, lawful code. So the widening is matched only as a **co-occurrence of
//! string payloads inside a rewrite-construction call** (class B), where the base
//! count is zero.
//!
//! `mutableType`'s `readonly` strip is class B for the same reason: the token is
//! only evidence when it is a payload being rewritten.
//!
//! # Formatting must not be an escape hatch
//!
//! Matching per line would be defeated by `rustfmt` breaking a method chain
//! across lines — that is, by ordinary formatting rather than by an attacker. So
//! the file is scanned once into a whitespace-free normalisation that keeps a
//! line number and an "is this character inside a string literal" flag per
//! character. Call sites, paren matching and class-A anchors are searched in that
//! normalisation; payloads are read from the string-literal runs inside it.
//!
//! Splitting the widening across two calls **in one file** is not an escape
//! hatch either (`Regex::new("\\bunknown\\b")` in one call, `.replace_all(t,
//! "any")` in another): payloads are pooled across the rewrite spans of that
//! file and the pair is judged over the pool.
//!
//! # The measured boundary — what this gate does NOT catch
//!
//! Stated exactly, because the R1 review found the earlier wording ("every
//! rewrite-construction span") claimed more than the code did, and the sole
//! carrier for pass 3 was defeatable through the gap. Every clause here is
//! pinned by `the_measured_lexical_boundary_is_what_the_docs_claim`, so it
//! cannot go stale silently.
//!
//! A "rewrite-construction span" means exactly a call to one of the enumerated
//! names in `REWRITE_CALLS`, in any receiver syntax, **with an optional
//! turbofish between the name and its argument list**, whose payloads are string
//! or char literals **written verbatim inside that call's own argument list**.
//!
//! The left edge is matched **per token family**, and the two families differ on
//! purpose (see [`LeftEdge`]): the `replace` family requires a non-identifier
//! before the name, so `send_replace(` and `str_replace(` are different
//! functions and open no span; the `Regex::new` family is matched by **suffix**,
//! so `regex::Regex::new`, `regex_lite::Regex::new` and `LazyRegex::new` are all
//! the same constructor. Both halves carry an executable control, because the
//! R2 review found this boundary asserted in prose, uncarried by any test, and
//! silently narrowing the `Regex::new` family that R1 had covered.
//!
//! Therefore:
//!
//! * **Const-hoisted payloads are not detected.** `const PAT: &str = "unknown";
//!   … t.replace(PAT, REP)` compiles and performs pass 3, but the literals are
//!   not inside the call. Nothing is *computed* here, so this is a distinct limit
//!   from a `format!`-assembled pattern, and both are out of reach.
//! * **A split across two FILES is not detected.** The contract is per-file by
//!   construction: each file is normalised and judged alone.
//! * **The pooled pair rule can synthesise a violation from two innocent calls
//!   in one file** — `t.replace("unknown", "unspecified")` beside
//!   `t.replace("all", "any")` reds with a pass-3 diagnostic. That is the price
//!   of closing the split-call evasion above, and it fails **closed**: it
//!   over-refuses rather than under-refuses. A lane that trips it needs a
//!   reviewed exemption, not a narrowing of this rule.
//! * **A lawful `readonly` rewrite reds as pass 5** — `t.replace("readonly",
//!   "ro")` is plausible in a SQLite or permission-mode context. Zero at base.
//!   Same standing as the `JsonValue` anchor below: reviewed exemption, never an
//!   unreviewed edit.
//!
//! Closing any of these needs dataflow or semantic-equivalence analysis. C3-4
//! forbids a *textual* reimplementation, and this gate is deliberately textual;
//! adding that machinery is outside its remit.
//!
//! # Coverage is bound to the workspace manifest, not to a directory walk
//!
//! The sibling architecture gates in this workspace
//! (`grapt-runtime/tests/no_unbounded_channels.rs` and friends) each scan their
//! OWN crate. C3-4's subject is *"no Rust crate"*, and a per-crate pattern
//! provably leaks here: `grapt-mail-transport` is the one member of 21 carrying
//! no `dependency_direction.rs`. A crate can land without its copy, and one did.
//!
//! So this gate enumerates roots from the **root manifest** — `members` plus
//! `exclude`, plus the shared `test-support/` sources that members pull in with
//! `#[path]` — and then walks the **whole repository** for `.rs` files and
//! REFUSES any file it cannot attribute to a declared root. A crate added
//! tomorrow is covered by construction: either it is declared, or its files are
//! unattributed and this gate fails.
//!
//! Nothing here asks Cargo to resolve anything, and no dependency edge is taken
//! on any sibling: crates are read as **files**. `dependency_direction.rs` stays
//! green, and this crate keeps its empty production dependency set.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[path = "no_generator_quirk_reimplementation/lexical.rs"]
mod lexical;
#[path = "no_generator_quirk_reimplementation/workspace.rs"]
mod workspace;

#[allow(unused_imports)]
use lexical::LeftEdge;
use lexical::{scan_source, Findings, LITERAL_ANCHORS, PAYLOAD_RULES};
use workspace::{
    declared_roots, detector_exclusion, scan_repository, self_exclusion, workspace_root,
    ScanReport, MIN_FILES_PER_ROOT, MIN_TOTAL_RUST_FILES,
};

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn no_rust_crate_reimplements_the_generator_textual_rewrites() {
    let root = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR")));
    let excluded = self_exclusion();
    let ScanReport {
        found,
        files,
        scanned,
        per_root,
    } = scan_repository(&root, Some(&excluded));

    // The floors. A walk that collected nothing finds no violations and would
    // otherwise report a clean run — see MIN_TOTAL_RUST_FILES.
    assert!(
        files >= MIN_TOTAL_RUST_FILES,
        "traversal collected {files} Rust files, below the floor of {MIN_TOTAL_RUST_FILES}; a \
         gate that scans nothing cannot be green"
    );
    assert!(
        scanned > 0,
        "every collected file was excluded from scanning"
    );
    for (root_path, count) in &per_root {
        assert!(
            *count >= MIN_FILES_PER_ROOT,
            "declared root {} contributed {count} Rust files, below the floor of \
             {MIN_FILES_PER_ROOT}",
            root_path.display()
        );
    }

    assert!(
        found.is_clean(),
        "C3-4: a Rust crate reimplements the generated-client emitter's textual rewrites.\n\
         Violations:\n{}\nRefusals:\n{}",
        found.violations.join("\n"),
        found.refusals.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The detector detects — hostile fixtures through the real scanner
// ---------------------------------------------------------------------------

/// Run the production scanner over crafted source. This is the same
/// `scan_source` the gate above calls, so a mutation that guts the detector
/// cannot leave these green.
fn judge(source: &str) -> Findings {
    let mut found = Findings::default();
    scan_source("fixture.rs", source, &mut found);
    found
}

/// Class A is one table-driven carrier, so a single mutation row cannot attribute
/// all five anchors. This test is that carrier's witness AND its matrix proof:
/// every anchor is exercised individually, so removing any one of them reds here
/// naming the pass that was lost.
#[test]
fn every_class_a_token_is_detected() {
    // The matrix half: each anchor alone, so no anchor rides on another's back.
    for (anchor, pass) in LITERAL_ANCHORS {
        let source = format!("const X: &str = \"{anchor}\";");
        assert!(
            !judge(&source).violations.is_empty(),
            "anchor `{anchor}` detects nothing on its own ({pass})"
        );
    }

    // One case per pass, in the shapes a port would actually take:
    // a bare token, a rewrite call, and a raw string.
    let class_a = [
        (
            "pass 1 brand erasure",
            r#"fn f(t: &str) -> String { t.replace(" & Brand.Brand<\"x\">", "") }"#,
        ),
        (
            "pass 2 alias, source side",
            r#"fn f(t: &str) -> String { t.replace("Schema.Json", "Value") }"#,
        ),
        (
            "pass 2 alias, target side",
            r##"fn f(t: &str) -> String { t.replace("X", r#"JsonValue"#) }"##,
        ),
        (
            "pass 4 autocomplete hack",
            r#"const SUGGEST: &str = "(string & {})";"#,
        ),
        (
            "pass 4 autocomplete hack, unspaced",
            r#"const SUGGEST: &str = "(string&{})";"#,
        ),
        (
            "pass 5 array rewrite",
            r#"fn f(t: &str) -> String { t.replace("ReadonlyArray<", "Array<") }"#,
        ),
    ];
    for (what, source) in class_a {
        let found = judge(source);
        assert!(
            !found.violations.is_empty(),
            "{what} was not detected: {found:?}"
        );
    }
}

/// R1 EP-C1's closure: the rewrite call is recognised by NAME, so every
/// fully-qualified spelling is a span.
///
/// This is a separate witness from the class B matrix on purpose. The defect it
/// guards is not "a payload rule is missing" but "the call was never collected,
/// so no rule could fire" — a different failure with a different cause, and
/// `QC10` attributes it here rather than sharing `QC05`'s record.
///
/// Every shape below was compiled and executed with `rustc --edition 2021`
/// before being written down: `str::replace("an unknown thing", "unknown",
/// "any")` evaluates to `"an any thing"`, so these are the emitter's pass 3 in
/// ordinary Rust, not lexical curiosities.
#[test]
fn the_widening_is_detected_in_every_fully_qualified_spelling() {
    let spellings = [
        (
            "str::replace",
            r#"fn f(t: &str) -> String { str::replace(t, "unknown", "any") }"#,
        ),
        (
            "String::replace",
            r#"fn f(t: &String) -> String { String::replace(t, "unknown", "any") }"#,
        ),
        (
            "str::replacen",
            r#"fn f(t: &str) -> String { str::replacen(t, "unknown", "any", 1) }"#,
        ),
        (
            "fully-qualified replace_all, pattern built separately",
            "fn f(t: &str) -> String {\n    let re = Regex::new(r\"\\bunknown\\b\");\n    Regex::replace_all(&re, t, \"any\").into()\n}",
        ),
        (
            "fully-qualified replacen_all, pattern built separately",
            "fn f(t: &str) -> String {\n    let re = Regex::new(r\"\\bunknown\\b\");\n    Regex::replacen_all(&re, t, 1, \"any\").into()\n}",
        ),
        (
            "fully qualified and rustfmt-broken across lines",
            "fn f(t: &str) -> String {\n    str::replace(\n        t,\n        \"unknown\",\n        \"any\",\n    )\n}",
        ),
        (
            "a bare call with no receiver at all",
            "fn f(t: &str) -> String {\n    let out = replace(t, \"unknown\", \"any\");\n    out\n}",
        ),
        // R2 DC-2b: the `Regex::new` family is matched by SUFFIX on purpose, so
        // a constructor spelled with a qualified type name is the same
        // constructor. R2 applied one identifier boundary to every token and
        // silently lost this shape; the policy is now per family and this case
        // is what holds it.
        (
            "a suffix-spelled regex constructor pooling into a later rewrite",
            "fn f(t: &str) -> String {\n    let re = LazyRegex::new(r\"\\bunknown\\b\");\n    re.replace_all(t, \"any\").to_string()\n}",
        ),
    ];
    for (what, source) in spellings {
        assert!(
            !judge(source).violations.is_empty(),
            "the fully-qualified spelling `{what}` was not detected — EP-C1 has reopened"
        );
    }

    // The two Regex spellings above carry `unknown` only in the separate
    // construction call, so it is the per-file pooling that convicts them.
    // Prove that rather than assume it: the same fully-qualified call with
    // innocent payloads and no pattern must stay green, or the cases above would
    // be passing for the wrong reason.
    assert!(
        judge("fn f(t: &str) -> String {\n    let re = Regex::new(r\"\\bfoo\\b\");\n    Regex::replace_all(&re, t, \"x\").into()\n}")
            .is_clean(),
        "a fully-qualified rewrite with innocent payloads was flagged"
    );
}

/// R2 DC-1's closure: a turbofish between the call name and its argument list
/// does not hide the call.
///
/// Its own witness rather than a case inside the fully-qualified one, because
/// the failure is different in kind: not "the receiver syntax was wrong" but
/// "something legal sat between the name and the paren". `QC11` attributes it
/// here.
///
/// Both shapes were compiled and executed with `rustc --edition 2021` before
/// being written down — `t.replace::<&str>("unknown", "any")` and
/// `str::replace::<&str>(t, "unknown", "any")` each evaluate to `"an any thing"`.
#[test]
fn the_widening_is_detected_through_a_turbofish() {
    let spellings = [
        (
            "method call with a turbofish",
            r#"fn f(t: &str) -> String { t.replace::<&str>("unknown", "any") }"#,
        ),
        (
            "fully-qualified call with a turbofish",
            r#"fn f(t: &str) -> String { str::replace::<&str>(t, "unknown", "any") }"#,
        ),
        (
            "a turbofish carrying nested angle brackets",
            r#"fn f(t: &str) -> String { t.replace::<Cow<'_, str>>("unknown", "any") }"#,
        ),
        (
            "a turbofish broken across lines by rustfmt",
            "fn f(t: &str) -> String {\n    t.replace::<\n        &str,\n    >(\"unknown\", \"any\")\n}",
        ),
    ];
    for (what, source) in spellings {
        assert!(
            !judge(source).violations.is_empty(),
            "the turbofish spelling `{what}` was not detected — DC-1 has reopened"
        );
    }

    // The `in_literal` guard inside the angle-depth scan, carried properly.
    //
    // R3 DC-3: this assertion previously put `"a<b"` in a SECOND FUNCTION, after
    // the turbofish had already balanced and the scan had already broken — so the
    // guard was never reached and the case passed whether or not the guard
    // existed. Neutering the guard left all twelve tests green. That is the same
    // wrong-reason shape this lane was held on one round earlier, inside the
    // correction that closed it.
    //
    // A string literal CAN sit between `::<` and its closing `>`, through a const
    // generic argument. This compiles and runs — `rustc --edition 2021` prints
    // `unknown->any#3`:
    //
    // ```
    // fn replace<const N: usize>(from: &str, to: &str) -> String { … }
    // replace::<{ "a>b".len() }>("unknown", "any")
    // ```
    //
    // With the guard, the `>` inside `"a>b"` is skipped, the real `>` closes the
    // depth, `(` is found and the span is opened. Without it, that `>` closes the
    // depth early, `at` lands mid-expression, the paren check rejects, and a
    // pass-3 span is silently lost. The guard is the sole carrier, so this
    // assertion is what makes it fail.
    assert!(
        !judge(
            "fn replace<const N: usize>(from: &str, to: &str) -> String { format!(\"{from}->{to}#{N}\") }\nfn f() -> String { replace::<{ \"a>b\".len() }>(\"unknown\", \"any\") }"
        )
        .violations
        .is_empty(),
        "a string literal containing `>` INSIDE the turbofish derailed the angle-depth skip \
         — the in_literal guard is gone"
    );
}

/// Class B is the other table-driven carrier: the widening pair and the readonly
/// strip share one code path, so this is its witness and its matrix proof.
#[test]
fn every_class_b_payload_rule_is_detected() {
    // The matrix half: each rule alone, in BOTH call spellings. R1 EP-C1 was
    // exactly a rule that held for the method form and not the fully-qualified
    // one, so a matrix that exercises only `t.replace(` re-opens it.
    let per_rule = [
        (
            "widening pair",
            r#"fn f(t: &str) -> String { t.replace("unknown", "any") }"#,
        ),
        (
            "widening pair, fully qualified",
            r#"fn f(t: &str) -> String { str::replace(t, "unknown", "any") }"#,
        ),
        (
            "readonly strip",
            r#"fn f(t: &str) -> String { t.replace("readonly ", "") }"#,
        ),
        (
            "readonly strip, fully qualified",
            r#"fn f(t: &str) -> String { str::replace(t, "readonly ", "") }"#,
        ),
    ];
    for (what, source) in per_rule {
        assert!(
            !judge(source).violations.is_empty(),
            "class B rule `{what}` detects nothing on its own"
        );
    }

    // The widening, wearing different formatting. Each of these is the same
    // defect, and the middle two are the ones a line-based matcher misses.
    let widening = [
        (
            "single line",
            "fn f(t: &str) -> String { t.replace(\"unknown\", \"any\") }",
        ),
        (
            "rustfmt-broken chain",
            "fn f(t: &str) -> String {\n    t\n        .replace(\n            \"unknown\",\n            \"any\",\n        )\n}",
        ),
        (
            "replacen",
            "fn f(t: &str) -> String { t.replacen(\"unknown\", \"any\", 1) }",
        ),
        (
            "split across a regex construction and its application",
            "fn f(t: &str) -> String {\n    let re = Regex::new(r\"\\bunknown\\b\");\n    re.replace_all(t, \"any\").into()\n}",
        ),
        (
            "trailing comment on the offending line",
            "fn f(t: &str) -> String { t.replace(\"unknown\", \"any\") } // widen like the emitter",
        ),
    ];
    for (what, source) in widening {
        let found = judge(source);
        assert!(
            !found.violations.is_empty(),
            "widening via {what} was not detected: {found:?}"
        );
    }
}

/// The measured lexical boundary, pinned rather than described.
///
/// R1 asked for this boundary to be stated precisely. Stating it in prose is
/// what produced EP-C1 in the first place — the module claimed "every
/// rewrite-construction span" and meant "every method-spelled one". So each
/// limit below is an assertion: if a later change closes one, this test fails and
/// the claim gets corrected instead of quietly going stale.
///
/// These are limits of a LEXICAL gate. Closing them needs dataflow or semantic
/// equivalence, which C3-4 does not ask for and this lane is forbidden to add.
#[test]
fn the_measured_lexical_boundary_is_what_the_docs_claim() {
    // EP-C2 — const-hoisted payloads are NOT detected. Compiles and behaves as
    // pass 3; the literals sit outside any rewrite span. This is NOT the
    // "computed values" residual the R1 receipt described, which is why it is
    // named separately.
    assert!(
        judge(
            "const PAT: &str = \"unknown\";\nconst REP: &str = \"any\";\nfn f(t: &str) -> String { t.replace(PAT, REP) }"
        )
        .is_clean(),
        "const-hoisted payloads are now detected — the documented boundary is stale"
    );

    // EP-C3 — a split across two FILES is not detected. The contract is
    // per-file by construction: each file is normalised and judged alone.
    assert!(
        judge("fn build() -> Regex { Regex::new(r\"\\bunknown\\b\") }").is_clean(),
        "one half of a two-file split reds on its own"
    );
    assert!(
        judge("fn apply(re: &Regex, t: &str) -> String { re.replace_all(t, \"any\").into() }")
            .is_clean(),
        "the other half of a two-file split reds on its own"
    );

    // EP-C4 — the pooled pair rule can synthesise a violation from two innocent
    // calls in one file. This is the price of closing the split-call evasion,
    // and it fails CLOSED: it over-refuses, never under-refuses. A lane that
    // trips it needs a reviewed exemption, not a silent narrowing.
    let synthesised = judge(
        r#"fn f(t: &str) -> String { t.replace("unknown", "unspecified").replace("all", "any") }"#,
    );
    assert!(
        !synthesised.violations.is_empty(),
        "the cross-span pooling no longer over-refuses; if that was deliberate, \
         the split-call witness must be re-proved"
    );

    // EP-C5 — a lawful `readonly` rewrite reds as pass 5. Plausible in a SQLite
    // or permission-mode context; zero at base. Same standing as the `JsonValue`
    // anchor: a reviewed exemption, never an unreviewed edit.
    assert!(
        !judge(r#"fn f(t: &str) -> String { t.replace("readonly", "ro") }"#)
            .violations
            .is_empty(),
        "the readonly rule no longer fires on a lawful rewrite — re-check pass 5 coverage"
    );
}

#[test]
fn lawful_rust_stays_green() {
    // The same-file control. Without it every witness above passes for the wrong
    // reason: a detector that refuses everything catches every violation and is
    // worthless. Each case below is drawn from code that ships in this workspace
    // or in the desktop shell today.
    let lawful = [
        (
            "desktop-style quote escaping",
            r#"fn q(c: &str) -> String { format!("\"{}\"", c.replace('"', "\"\"")) }"#,
        ),
        (
            "path separator normalisation",
            r#"fn p(d: &str) -> String { d.replace('\\', "/") }"#,
        ),
        (
            "Option::replace, which is not a string rewrite at all",
            "fn f(slot: &mut Option<u8>, v: u8) -> bool { slot.replace(v).is_some() }",
        ),
        (
            "an unrelated unknown literal",
            r#"fn f() -> &'static str { "unknown" }"#,
        ),
        (
            "an unrelated any literal",
            r#"fn f() -> &'static str { "any" }"#,
        ),
        (
            "unknown and any as ordinary words, far from any rewrite",
            "fn f(xs: &[u8]) -> bool {\n    // an unknown element makes any check fail\n    xs.iter().any(|x| *x == 0)\n}",
        ),
        (
            "a rewrite whose payloads are innocent",
            r#"fn f(t: &str) -> String { t.replace("disposition complete", "disposition truncated") }"#,
        ),
        (
            "a runtime-sourced regex with no literal payload",
            "fn f(src: &str) -> bool { Regex::new(src).is_ok() }",
        ),
        // Both of these are real shipped shapes in this workspace, and both
        // became relevant only when EP-C1's closure widened the match from the
        // method spelling to the call name.
        (
            "core::mem::replace, a fully-qualified call that is not a text rewrite \
             (crates/grapt-artifact/src/digest.rs:115)",
            "fn f(slot: &mut Vec<u8>, fresh: Vec<u8>) -> Vec<u8> { core::mem::replace(slot, fresh) }",
        ),
        (
            "send_replace, a differently-named function the boundary check excludes \
             (crates/grapt-runtime/src/coordinator.rs)",
            "fn f(tx: &Sender<u8>) { tx.send_replace(7); }",
        ),
        (
            "a wrapper named for the thing it wraps",
            r#"fn str_replace(t: &str) -> String { t.to_owned() }"#,
        ),
        // THE carrier for the replace-family left boundary, and the reason the
        // two cases above are not.
        //
        // R2 DC-2a: R2 cited `core::mem::replace`, `tx.send_replace(7)` and a
        // `str_replace` wrapper as proof the boundary was load-bearing. None of
        // them carries a string payload, so all three pass identically whether
        // or not the guard exists — witnesses that could never fire. Deleting
        // the boundary at R2 left all eleven tests green.
        //
        // This one pools two payloads through a differently-named function. With
        // the boundary it is two calls to `send_replace` and no span at all;
        // without it, `unknown` and `any` land in the same pool and the pair rule
        // convicts lawful code. It is green here and red the moment the guard
        // goes — which is what `QC12` attributes.
        (
            "send_replace carrying status strings that would pool without the boundary",
            "fn f(tx: &Sender<String>) {\n    tx.send_replace(\"unknown\".to_string());\n    tx.send_replace(\"any\".to_string());\n}",
        ),
        // THE carrier for the right-edge `(` requirement (R3 arm F, uncarried).
        //
        // The name is MENTIONED, not called: after it comes `;`, not `(`, so no
        // span opens. Drop the right-edge check and the site is accepted anyway;
        // the span then walks forward to the next parenthesised group — the
        // returned tuple — and its two literals pool into a pass-3 conviction of
        // lawful code.
        //
        // Note the shape carefully. A first attempt used `let replace = (…)`,
        // which cannot carry this at all: whitespace is stripped before matching,
        // so `let replace` becomes `letreplace` and the LEFT boundary rejects it
        // before the right edge is ever consulted. The mention must be preceded
        // by a non-identifier character — here `=` — for the right edge to be the
        // only thing standing in the way.
        //
        // The check is FAIL-CLOSED and MONOTONE: removing it can only ADD call
        // sites, never remove one, so it cannot hide a violation. That is why it
        // was not blocking. It can only cost precision — and precision is exactly
        // what this control measures.
        (
            "a lawful mention of the name, not a call",
            "fn replace(t: &str) -> String { t.to_owned() }\nfn f() -> (&'static str, &'static str) {\n    let _f: fn(&str) -> String = replace;\n    (\"unknown\", \"any\")\n}",
        ),
        (
            "prose naming every forbidden token, in comments",
            "// Schema.Json becomes JsonValue, unknown becomes any, ReadonlyArray< is rewritten,\n// and (string & {}) preserves autocomplete. None of this is code.\n/* Brand.Brand<\"x\"> too. */\nfn f() {}",
        ),
    ];
    for (what, source) in lawful {
        let found = judge(source);
        assert!(
            found.is_clean(),
            "lawful code was flagged ({what}): {found:?}"
        );
    }
}

#[test]
fn a_trailing_comment_does_not_hide_the_code_before_it() {
    // The precedent gates skip a line whose trimmed form starts with `//`. That
    // is right for them and would be a hole here, so the distinction gets its own
    // witness: prose is inert, and code carrying prose is not.
    let hidden =
        "fn f(t: &str) -> String { t.replace(\"ReadonlyArray<\", \"Array<\") } // harmless";
    assert!(
        !judge(hidden).violations.is_empty(),
        "a trailing comment hid the code before it"
    );

    let prose = "// t.replace(\"ReadonlyArray<\", \"Array<\") would be forbidden\nfn f() {}";
    assert!(
        judge(prose).is_clean(),
        "a comment describing the defect was treated as the defect"
    );
}

/// R1 EP-C6: the refusal branch, end to end, through the shipped pipeline.
///
/// `the_coverage_binding_refuses_an_undeclared_source_tree` exercises the roots
/// *computation*; this drives `scan_repository` itself over a fixture repository
/// built in the OS temp directory — never inside either repository, because a
/// battery is void if a lane writes into the repo while it runs. A planted `.rs`
/// outside `members ∪ exclude ∪ test-support` must be refused by name, and the
/// same fixture must come back clean once it is removed.
#[test]
fn an_undeclared_source_file_is_refused_end_to_end() {
    let fixture = std::env::temp_dir().join(format!(
        "grapt-c34-refusal-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = fs::remove_dir_all(&fixture);

    let declared = fixture.join("crates").join("a").join("src");
    let undeclared = fixture.join("newcrate").join("src");
    for dir in [&declared, &undeclared, &fixture.join("test-support")] {
        if let Err(error) = fs::create_dir_all(dir) {
            panic!("cannot build the fixture at {}: {error}", dir.display());
        }
    }
    let write = |path: &Path, text: &str| {
        if let Err(error) = fs::write(path, text) {
            panic!("cannot write {}: {error}", path.display());
        }
    };
    write(
        &fixture.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/a\"]\n",
    );
    write(&declared.join("lib.rs"), "pub fn ok() {}\n");
    write(
        &fixture.join("test-support").join("shared.rs"),
        "pub fn s() {}\n",
    );

    // Guard the guard: with nothing planted the fixture must be clean, or the
    // refusal below could be coming from anything.
    let clean = scan_repository(&fixture, None);
    assert!(
        clean.found.is_clean(),
        "the fixture repository was not clean before planting: {:?}",
        clean.found
    );
    assert_eq!(clean.files, 2, "expected exactly the two declared sources");

    // Plant a Rust source under no declared root.
    let planted = undeclared.join("lib.rs");
    write(&planted, "pub fn sneaky() {}\n");
    let report = scan_repository(&fixture, None);
    assert!(
        report.found.refusals.iter().any(|refusal| refusal
            .contains("under no declared workspace root")
            && refusal.contains("lib.rs")),
        "an undeclared Rust source was not refused: {:?}",
        report.found
    );
    assert!(
        report.found.violations.is_empty(),
        "the planted file is a refusal, not a rewrite violation: {:?}",
        report.found
    );

    // And green again once it is gone, so the refusal is attributable to the
    // plant rather than to the fixture.
    if let Err(error) = fs::remove_dir_all(fixture.join("newcrate")) {
        panic!("cannot remove the plant: {error}");
    }
    let after = scan_repository(&fixture, None);
    assert!(
        after.found.is_clean(),
        "the fixture did not return clean after the plant was removed: {:?}",
        after.found
    );

    let _ = fs::remove_dir_all(&fixture);
}

#[test]
fn the_coverage_binding_refuses_an_undeclared_source_tree() {
    // The gate's coverage comes from the root manifest, not from the directory
    // walk. This proves the two are actually compared: a Rust file under no
    // declared root is refused rather than skipped, which is what makes a crate
    // added tomorrow covered by construction.
    let root = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR")));
    let roots = declared_roots(&root);
    assert!(
        roots.len() >= 22,
        "expected the 21 members plus exclusions and shared sources, found {}",
        roots.len()
    );

    let intruder = root
        .join("crates-that-do-not-exist")
        .join("src")
        .join("lib.rs");
    assert!(
        !roots.iter().any(|declared| intruder.starts_with(declared)),
        "a path under no declared root was attributed to one"
    );

    // And the control: a real file IS attributed, so the check above cannot pass
    // by attributing nothing.
    let real = root
        .join("crates")
        .join("grapt-types")
        .join("src")
        .join("lib.rs");
    assert!(
        roots.iter().any(|declared| real.starts_with(declared)),
        "a declared crate's source was not attributed to any root"
    );
}

#[test]
fn the_self_exclusion_is_exactly_one_named_file() {
    // A wildcard exclusion would exempt every test in the workspace. This pins
    // the exemption to one path and proves it is this file.
    let excluded = self_exclusion();
    assert!(
        excluded.ends_with("tests/no_generator_quirk_reimplementation.rs")
            || excluded.ends_with("tests\\no_generator_quirk_reimplementation.rs"),
        "the self-exclusion names an unexpected file: {}",
        excluded.display()
    );

    // Sibling tests in this very directory are NOT excluded.
    let sibling = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("dependency_direction.rs");
    assert_ne!(
        excluded, sibling,
        "the exclusion must not reach a sibling test"
    );
    assert!(
        sibling.is_file(),
        "expected a sibling test to exist as the control"
    );

    // The split detector owns the forbidden constants and therefore needs its
    // own exact-file exclusion. The workspace helper remains scanned, and no
    // directory or sibling test is exempted.
    let detector = detector_exclusion();
    assert!(
        detector.ends_with("tests/no_generator_quirk_reimplementation/lexical.rs")
            || detector.ends_with("tests\\no_generator_quirk_reimplementation\\lexical.rs"),
        "the detector exclusion names an unexpected file: {}",
        detector.display()
    );
    assert_ne!(
        detector, sibling,
        "the detector exclusion reached a sibling"
    );
    assert_ne!(
        detector, excluded,
        "the facade and detector exclusions must be separately exact"
    );
}

#[test]
fn the_forbidden_set_covers_all_five_passes_and_nothing_more() {
    // The scope pin. The ruling of 2026-08-14 fixes the forbidden set at the
    // five §3.3 passes and excludes §3.1's seed-plus-suffix numbering, so a later
    // widening or narrowing of the table has to come through this assertion
    // rather than through an unreviewed edit. The per-entry matrix proofs live
    // with their carriers, in the two detection tests above.
    let anchors: BTreeSet<&str> = LITERAL_ANCHORS.iter().map(|(a, _)| *a).collect();
    let expected: BTreeSet<&str> = [
        "Brand.Brand<",
        "Schema.Json",
        "JsonValue",
        "(string & {})",
        "ReadonlyArray<",
    ]
    .into_iter()
    .collect();
    assert_eq!(anchors, expected, "the class A anchor set drifted");
    assert_eq!(
        PAYLOAD_RULES.len(),
        2,
        "expected the widening pair and the readonly strip"
    );
}
