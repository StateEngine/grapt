//! Workspace-wide shrink-only ratchet for `#[expect(..., reason = "legacy")]`.
//!
//! Legacy lint-suppression markers exist only for pre-existing debt and may
//! never be newly added, grown, or swapped for a different lint. Existing
//! per-file lint counts are recorded in `legacy-expect-ratchet.json`; as
//! markers are retired, lower or remove the lint/file entry in the same change.
//!
//! This gate reads only the current workspace tree — never git history — so
//! shallow clones and post-scrub trees pass identically. Monotonic shrinking
//! against the base commit is enforced in CI by `scripts/build/ratchet_monotonic.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "no_god_files/scan.rs"]
mod scan;

use scan::scan_legacy_expects;
use serde_json::Value;

const RATCHET: &str = include_str!("legacy-expect-ratchet.json");
const RATCHET_PATH: &str = "crates/grapt-types/tests/legacy-expect-ratchet.json";

type LintCounts = BTreeMap<String, usize>;
type Inventory = BTreeMap<String, LintCounts>;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_ratchet(source: &str) -> Result<Inventory, String> {
    let document: Value =
        serde_json::from_str(source).map_err(|error| format!("{RATCHET_PATH}: {error}"))?;
    let files = document
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{RATCHET_PATH}: files must be an object"))?;
    let mut out = Inventory::new();
    for (path, raw_lints) in files {
        if path.contains('\\') || path.starts_with('/') || path.contains("..") {
            return Err(format!(
                "{RATCHET_PATH}: {path} must be a normalized repository-relative path"
            ));
        }
        let lint_map = raw_lints
            .as_object()
            .ok_or_else(|| format!("{RATCHET_PATH}: {path} must map lint names to counts"))?;
        if lint_map.is_empty() {
            return Err(format!(
                "{RATCHET_PATH}: {path} has no lint entries; delete the empty file entry"
            ));
        }
        let mut counts = LintCounts::new();
        for (lint, raw_count) in lint_map {
            if lint.trim().is_empty() || lint != lint.trim() {
                return Err(format!(
                    "{RATCHET_PATH}: {path} has invalid lint name {lint:?}"
                ));
            }
            let count = raw_count
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    format!("{RATCHET_PATH}: {path} lint {lint} must have a positive integer count")
                })?;
            counts.insert(lint.clone(), count);
        }
        out.insert(path.clone(), counts);
    }
    Ok(out)
}

fn entry_is_link_or_reparse(path: &Path, kind: fs::FileType) -> Result<bool, String> {
    if kind.is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(false)
    }
}

fn collect_rust_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|error| format!("cannot read {}: {error}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        if entry_is_link_or_reparse(&path, kind)? {
            return Err(format!(
                "source census refuses link or reparse point {}",
                path.display()
            ));
        }
        if kind.is_dir() {
            collect_rust_files(root, &path, files)?;
        } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("cannot relativize {}: {error}", path.display()))?;
            files.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn legacy_expect_inventory(root: &Path) -> Result<Inventory, String> {
    let crates = root.join("crates");
    let crate_entries = fs::read_dir(&crates)
        .map_err(|error| format!("cannot read {}: {error}", crates.display()))?;
    let mut files = Vec::new();
    for entry in crate_entries {
        let entry = entry.map_err(|error| format!("cannot read crate entry: {error}"))?;
        let crate_root = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| format!("cannot stat {}: {error}", crate_root.display()))?;
        if entry_is_link_or_reparse(&crate_root, kind)? {
            return Err(format!(
                "source census refuses link or reparse-point crate root {}",
                crate_root.display()
            ));
        }
        if kind.is_dir() {
            collect_rust_files(root, &crate_root, &mut files)?;
        }
    }
    files.sort();

    let mut inventory = Inventory::new();
    for relative in files {
        let path = root.join(&relative);
        let source =
            fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let counts = scan_legacy_expects(&source)
            .map_err(|error| format!("cannot scan {normalized}: {error}"))?;
        if !counts.is_empty() {
            inventory.insert(normalized, counts);
        }
    }
    Ok(inventory)
}

fn evaluate(inventory: &Inventory, baseline: &Inventory) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    let mut seen_files = BTreeSet::new();
    for (path, lints) in inventory {
        let Some(recorded_lints) = baseline.get(path) else {
            failures.push(format!(
                "{path}: has new reason = \"legacy\" #[expect] marker(s) ({lints:?}); never add a new legacy marker"
            ));
            continue;
        };
        seen_files.insert(path.as_str());
        for (lint, count) in lints {
            match recorded_lints.get(lint) {
                Some(recorded) if count > recorded => failures.push(format!(
                    "{path}: reason = \"legacy\" #[expect({lint})] grew from {recorded} to {count}; never add a new legacy marker"
                )),
                Some(recorded) if count < recorded => failures.push(format!(
                    "{path}: reason = \"legacy\" #[expect({lint})] shrank from {recorded} to {count}; lower its entry in {RATCHET_PATH}"
                )),
                Some(_) => {}
                None => failures.push(format!(
                    "{path}: new reason = \"legacy\" #[expect({lint})] marker; swapping or adding a legacy lint is forbidden"
                )),
            }
        }
        for lint in recorded_lints.keys() {
            if !lints.contains_key(lint) {
                failures.push(format!(
                    "{path}: no longer has reason = \"legacy\" #[expect({lint})]; remove it from {RATCHET_PATH}"
                ));
            }
        }
    }
    for path in baseline.keys() {
        if !seen_files.contains(path.as_str()) {
            failures.push(format!(
                "{path}: no longer has any reason = \"legacy\" #[expect] markers; delete it from {RATCHET_PATH}"
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

#[test]
fn legacy_expect_markers_only_shrink() -> Result<(), Box<dyn Error>> {
    let root = workspace_root();
    let baseline = parse_ratchet(RATCHET)?;
    let inventory = legacy_expect_inventory(&root)?;
    evaluate(&inventory, &baseline).map_err(|failures| failures.join("\n").into())
}

#[test]
fn ast_parser_decodes_escaped_reasons_raw_keys_and_item_identities() -> Result<(), Box<dyn Error>> {
    let source = concat!(
        "// #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "/* #[expect(clippy::too_many_lines, reason = \"legacy\")] */\n",
        "const NOTE: &str = \"#[expect(clippy::too_many_lines, reason = \\\"legacy\\\")]\";\n",
        "const RAW: &str = r#\"#[expect(clippy::too_many_lines, reason = \"legacy\")]\"#;\n",
        "#[expect(clippy::too_many_lines, reason = \"leg\\x61cy\")]\n",
        "fn escaped_hex() {}\n",
        "#[expect(clippy::cognitive_complexity, r#reason = \"leg\\u{61}cy\")]\n",
        "fn raw_reason_and_unicode() {}\n",
        "#[cfg_attr(test, expect(clippy::too_many_lines, reason = \"legacy\"))]\n",
        "fn cfg_wrapped() {}\n",
        "#[expect(clippy::too_many_lines, reason = \"justified\")]\n",
        "fn non_legacy() {}\n",
    );
    let counts = scan_legacy_expects(source.as_bytes())?;
    assert_eq!(
        counts,
        BTreeMap::from([
            ("fn escaped_hex :: clippy::too_many_lines".to_owned(), 1),
            (
                "fn raw_reason_and_unicode :: clippy::cognitive_complexity".to_owned(),
                1
            ),
            ("fn cfg_wrapped :: clippy::too_many_lines".to_owned(), 1),
        ])
    );
    Ok(())
}

#[test]
fn new_growth_item_swap_and_stale_legacy_entries_are_refused() {
    let path = "crates/example/src/lib.rs".to_owned();
    let base = BTreeMap::from([(
        path.clone(),
        BTreeMap::from([("fn alpha :: clippy::too_many_lines".to_owned(), 1)]),
    )]);
    let moved_item = BTreeMap::from([(
        path.clone(),
        BTreeMap::from([("fn beta :: clippy::too_many_lines".to_owned(), 1)]),
    )]);
    let swapped_lint = BTreeMap::from([(
        path.clone(),
        BTreeMap::from([("fn alpha :: clippy::cognitive_complexity".to_owned(), 1)]),
    )]);
    let grown = BTreeMap::from([(
        path.clone(),
        BTreeMap::from([("fn alpha :: clippy::too_many_lines".to_owned(), 2)]),
    )]);
    let stale = BTreeMap::new();
    let new_file = BTreeMap::from([
        (
            path.clone(),
            BTreeMap::from([("fn alpha :: clippy::too_many_lines".to_owned(), 1)]),
        ),
        (
            "crates/example/src/new.rs".to_owned(),
            BTreeMap::from([("fn gamma :: clippy::too_many_lines".to_owned(), 1)]),
        ),
    ]);
    assert!(evaluate(&moved_item, &base)
        .is_err_and(|f| f.iter().any(|line| line.contains("swapping or adding"))));
    assert!(evaluate(&swapped_lint, &base)
        .is_err_and(|f| f.iter().any(|line| line.contains("swapping or adding"))));
    assert!(evaluate(&grown, &base).is_err_and(|f| f.iter().any(|line| line.contains("grew"))));
    assert!(evaluate(&stale, &base)
        .is_err_and(|f| f.iter().any(|line| line.contains("no longer has any"))));
    assert!(evaluate(&new_file, &base)
        .is_err_and(|f| f.iter().any(|line| line.contains("has new reason"))));
}

#[test]
fn moving_legacy_expect_between_same_named_methods_in_different_impls_is_refused(
) -> Result<(), Box<dyn Error>> {
    let path = "crates/example/src/lib.rs".to_owned();
    let before = concat!(
        "mod m {\n",
        "    impl Foo {\n",
        "        #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "        fn run(&self) {}\n",
        "    }\n",
        "    impl Bar {\n",
        "        fn run(&self) {}\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "mod m {\n",
        "    impl Foo {\n",
        "        fn run(&self) {}\n",
        "    }\n",
        "    impl Bar {\n",
        "        #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "        fn run(&self) {}\n",
        "    }\n",
        "}\n",
    );
    let before_counts = scan_legacy_expects(before.as_bytes())?;
    let after_counts = scan_legacy_expects(after.as_bytes())?;
    assert_eq!(
        before_counts,
        BTreeMap::from([(
            "mod m::impl Foo::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert_eq!(
        after_counts,
        BTreeMap::from([(
            "mod m::impl Bar::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert!(evaluate(
        &BTreeMap::from([(path.clone(), after_counts)]),
        &BTreeMap::from([(path.clone(), before_counts)])
    )
    .is_err_and(|f| f.iter().any(|line| line.contains("swapping or adding"))));

    let gen_u8 = concat!(
        "impl Wrapper<u8> {\n",
        "    #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "    fn run(&self) {}\n",
        "}\n",
        "impl Wrapper<u16> {\n",
        "    fn run(&self) {}\n",
        "}\n",
    );
    let gen_u16 = concat!(
        "impl Wrapper<u8> {\n",
        "    fn run(&self) {}\n",
        "}\n",
        "impl Wrapper<u16> {\n",
        "    #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "    fn run(&self) {}\n",
        "}\n",
    );
    let u8_counts = scan_legacy_expects(gen_u8.as_bytes())?;
    let u16_counts = scan_legacy_expects(gen_u16.as_bytes())?;
    assert_eq!(
        u8_counts,
        BTreeMap::from([(
            "impl Wrapper < u8 >::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert_eq!(
        u16_counts,
        BTreeMap::from([(
            "impl Wrapper < u16 >::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert!(evaluate(
        &BTreeMap::from([(path.clone(), u16_counts)]),
        &BTreeMap::from([(path, u8_counts)])
    )
    .is_err_and(|f| f.iter().any(|line| line.contains("swapping or adding"))));
    Ok(())
}

#[test]
fn const_generics_where_clauses_and_macro_tokens_preserve_impl_scope() -> Result<(), Box<dyn Error>>
{
    let path = "crates/example/src/lib.rs".to_owned();
    let const_gen_u8 = concat!(
        "impl<const N: usize> Wrapper<N, u8> where [u8; N]: Sized {\n",
        "    #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "    fn run(&self) {}\n",
        "}\n",
        "impl<const N: usize> Wrapper<N, u16> where [u16; N]: Sized {\n",
        "    fn run(&self) {}\n",
        "}\n",
    );
    let const_gen_u16 = concat!(
        "impl<const N: usize> Wrapper<N, u8> where [u8; N]: Sized {\n",
        "    fn run(&self) {}\n",
        "}\n",
        "impl<const N: usize> Wrapper<N, u16> where [u16; N]: Sized {\n",
        "    #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "    fn run(&self) {}\n",
        "}\n",
    );
    let const_u8_counts = scan_legacy_expects(const_gen_u8.as_bytes())?;
    let const_u16_counts = scan_legacy_expects(const_gen_u16.as_bytes())?;
    assert_eq!(
        const_u8_counts,
        BTreeMap::from([(
            "impl Wrapper < N , u8 >::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert_eq!(
        const_u16_counts,
        BTreeMap::from([(
            "impl Wrapper < N , u16 >::fn run :: clippy::too_many_lines".to_owned(),
            1
        )])
    );
    assert!(evaluate(
        &BTreeMap::from([(path.clone(), const_u16_counts)]),
        &BTreeMap::from([(path, const_u8_counts)])
    )
    .is_err_and(|f| f.iter().any(|line| line.contains("swapping or adding"))));

    let tricky = concat!(
        "const MACRO_STR: &str = stringify!(impl <);\n",
        "enum Kind {\n",
        "    #[expect(clippy::large_enum_variant, reason = \"legacy\")]\n",
        "    Huge([u8; 4096]),\n",
        "}\n",
        "struct Pair {\n",
        "    #[expect(dead_code, reason = \"legacy\")]\n",
        "    left: u8,\n",
        "    #[expect(dead_code, reason = \"legacy\")]\n",
        "    right: u8,\n",
        "}\n",
        "struct TuplePair(#[expect(dead_code, reason = \"legacy\")] u8, u8);\n",
        "macro_rules! gen_fn {\n",
        "    () => {\n",
        "        #[inline]\n",
        "        #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "        fn generated() {}\n",
        "    };\n",
        "}\n",
        "macro_rules! nested_stringify {\n",
        "    () => {\n",
        "        stringify!(#[expect(dead_code, reason = \"legacy\")]);\n",
        "        ::core::stringify!(#[expect(dead_code, reason = \"legacy\")]);\n",
        "        std::stringify!(#[expect(dead_code, reason = \"legacy\")]);\n",
        "        my_mod::stringify!(#[expect(dead_code, reason = \"legacy\")]);\n",
        "    };\n",
        "}\n",
        "impl<T> Trait<u8> for Wrapper<T, u8> where T: Fn() -> u8, (): Flag<{ 1 < 2 }> {\n",
        "    #[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "    fn run(&self) {}\n",
        "}\n",
        "#[expect(clippy::too_many_lines, reason = \"legacy\")]\n",
        "fn after_const_expr() {}\n",
    );
    assert_eq!(
        scan_legacy_expects(tricky.as_bytes())?,
        BTreeMap::from([
            (
                "enum Kind::variant Huge :: clippy::large_enum_variant".to_owned(),
                1
            ),
            ("struct Pair::field left :: dead_code".to_owned(), 1),
            ("struct Pair::field right :: dead_code".to_owned(), 1),
            ("struct TuplePair::field 0 :: dead_code".to_owned(), 1),
            ("macro gen_fn :: clippy::too_many_lines".to_owned(), 1),
            ("macro nested_stringify :: dead_code".to_owned(), 4),
            (
                "impl Trait < u8 > for Wrapper < T , u8 >::fn run :: clippy::too_many_lines"
                    .to_owned(),
                1
            ),
            (
                "fn after_const_expr :: clippy::too_many_lines".to_owned(),
                1
            ),
        ])
    );
    Ok(())
}

#[test]
fn macro_tokens_scan_fail_closed_and_impl_keys_preserve_token_boundaries(
) -> Result<(), Box<dyn Error>> {
    // All macro invocations are scanned fail-closed (no heuristic `stringify!` skip),
    // while real string literals are ignored because they tokenize as literals.
    let macros_and_strings = concat!(
        "const LITERAL: &str = \"#[expect(dead_code, reason = \\\"legacy\\\")]\";\n",
        "macro_rules! stringify { ($($t:tt)*) => { $($t)* }; }\n",
        "stringify!(#[expect(dead_code, reason = \"legacy\")] fn a() {});\n",
        "core::stringify!(#[expect(dead_code, reason = \"legacy\")] fn b() {});\n",
        "my_mod::stringify!(#[expect(dead_code, reason = \"legacy\")] fn c() {});\n",
    );
    assert_eq!(
        scan_legacy_expects(macros_and_strings.as_bytes())?,
        BTreeMap::from([("macro :: dead_code".to_owned(), 3)])
    );

    // Impl keys preserve both literal-internal whitespace and inter-token boundaries.
    for (left, right) in [
        (
            "impl Wrapper<{ \"a b\".len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
            "impl Wrapper<{ \"ab\".len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
        ),
        (
            "impl Wrapper<{ A as usize }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
            "impl Wrapper<{ Aasusize }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
        ),
        (
            "impl Wrapper<{ stringify!(a b).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
            "impl Wrapper<{ stringify!(ab).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
        ),
        (
            "impl Wrapper<{ stringify!(..).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
            "impl Wrapper<{ stringify!(. .).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
        ),
        (
            "impl Wrapper<{ stringify!(a+=b).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
            "impl Wrapper<{ stringify!(a + = b).len() }> { #[expect(clippy::too_many_lines, reason = \"legacy\")] fn run(&self) {} }",
        ),
    ] {
        let left_counts = scan_legacy_expects(left.as_bytes())?;
        let right_counts = scan_legacy_expects(right.as_bytes())?;
        assert_eq!(left_counts.len(), 1);
        assert_eq!(right_counts.len(), 1);
        assert_ne!(left_counts, right_counts, "keys collided for:\n{left}\n{right}");
    }
    Ok(())
}
