//! Zero-tolerance ban on `include!()` anywhere under `crates/*/src/`.
//!
//! Stitching Rust source together with `include!` hides real module boundaries
//! and gives parts no interface. Zero `include!()` sites are permitted anywhere
//! under `crates/*/src/`, including `#[cfg(test)]` modules; use ordinary
//! submodules with real interfaces instead.
use super::*;

const RATCHET: &str = "crates/grapt-types/tests/include-ratchet.json";

fn is_production_src(path: &str) -> bool {
    let mut parts = path.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("crates"), Some(_), Some("src"), Some(_))
    )
}

struct IncludeTokenScan {
    count: usize,
}

impl syn::parse::Parse for IncludeTokenScan {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut count = 0usize;
        let mut prev_was_include = false;
        while !input.is_empty() {
            if prev_was_include && input.peek(syn::Token![!]) {
                let _bang: syn::Token![!] = input.parse()?;
                if input.peek(syn::token::Paren)
                    || input.peek(syn::token::Bracket)
                    || input.peek(syn::token::Brace)
                {
                    count = count.saturating_add(1);
                }
                prev_was_include = false;
                continue;
            }
            if input.peek(syn::token::Paren) {
                let inner;
                syn::parenthesized!(inner in input);
                count = count.saturating_add(inner.parse::<Self>()?.count);
                prev_was_include = false;
            } else if input.peek(syn::token::Bracket) {
                let inner;
                syn::bracketed!(inner in input);
                count = count.saturating_add(inner.parse::<Self>()?.count);
                prev_was_include = false;
            } else if input.peek(syn::token::Brace) {
                let inner;
                syn::braced!(inner in input);
                count = count.saturating_add(inner.parse::<Self>()?.count);
                prev_was_include = false;
            } else if let Ok(ident) = input.parse::<syn::Ident>() {
                prev_was_include = ident.to_string().trim_start_matches("r#") == "include";
            } else {
                input.step(|cursor| match cursor.token_tree() {
                    Some((_, next)) => Ok(((), next)),
                    None => Err(cursor.error("unexpected end of token stream")),
                })?;
                prev_was_include = false;
            }
        }
        Ok(Self { count })
    }
}

fn scan_src_includes(files: &BTreeMap<String, Vec<u8>>) -> Result<BTreeMap<String, usize>, String> {
    let mut found = BTreeMap::new();
    for (path, bytes) in files {
        if !is_production_src(path) {
            continue;
        }
        let src = std::str::from_utf8(bytes)
            .map_err(|_| format!("{path}: production src/ file is not valid UTF-8"))?;
        let scan: IncludeTokenScan = syn::parse_str(src)
            .map_err(|error| format!("{path}: failed to tokenize Rust source: {error}"))?;
        if scan.count > 0 {
            found.insert(path.clone(), scan.count);
        }
    }
    Ok(found)
}

fn ensure_ratchet_empty(root: &Path) -> Result<(), String> {
    let bytes = read_bounded(&root.join(RATCHET), MAX_FILE_BYTES)?;
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{RATCHET}: {error}"))?;
    let carriers = document["carriers"]
        .as_object()
        .ok_or_else(|| format!("{RATCHET}: carriers must be an object"))?;
    if !carriers.is_empty() {
        return Err(format!(
            "{RATCHET}: carriers must remain empty; zero include!() sites are permitted under crates/*/src/"
        ));
    }
    Ok(())
}

#[test]
fn production_src_include_sites_only_shrink() -> Result<(), Box<dyn Error>> {
    let _tree = lock_tree();
    let root = workspace_root();
    ensure_ratchet_empty(&root)?;
    let inventory = inventory(&root)?;
    let found = scan_src_includes(&inventory.files)?;
    if found.is_empty() {
        return Ok(());
    }
    let problems: Vec<String> = found
        .into_iter()
        .map(|(carrier, count)| {
            format!(
                "{carrier}: {count} include!() site(s) in production src/; use a submodule with a real interface"
            )
        })
        .collect();
    Err(bounded_diagnostic(
        "include! ban: ",
        &problems.iter().map(String::as_str).collect::<Vec<_>>(),
    )?
    .into())
}

#[test]
fn token_scanner_catches_src_includes_across_lifetimes_unicode_escapes_and_groups(
) -> Result<(), Box<dyn Error>> {
    // Ignores crates/*/tests/ and comments/strings, catches crates/*/src/.
    let mut files = BTreeMap::new();
    files.insert(
        "crates/example/src/clean.rs".to_owned(),
        b"// include!(\"ignored.rs\");\nconst S: &str = \"include!(\\\"ignored.rs\\\");\";\nfn f() { let _ = ['\\u{a________________}', '\"']; }\n".to_vec(),
    );
    files.insert(
        "crates/example/tests/suite.rs".to_owned(),
        b"include!(\"helper.rs\");\n".to_vec(),
    );
    assert!(scan_src_includes(&files)?.is_empty());

    // Two lifetimes on one line cannot hide include!().
    let mut same_line_lifetimes = BTreeMap::new();
    same_line_lifetimes.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"fn f<'a>() { include!(\"part.rs\"); let _: &'a str = \"\"; }\n".to_vec(),
    );
    assert_eq!(
        scan_src_includes(&same_line_lifetimes)?.get("crates/example/src/lib.rs"),
        Some(&1)
    );

    // Long Unicode escape followed by quote char literal cannot hide include!().
    let mut unicode_escape = BTreeMap::new();
    unicode_escape.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"fn f() { let _ = ['\\u{a________________}','\"']; include!(\"part.rs\"); }\n".to_vec(),
    );
    assert_eq!(
        scan_src_includes(&unicode_escape)?.get("crates/example/src/lib.rs"),
        Some(&1)
    );

    // Macro definitions and nested groups are also traversed.
    let mut inside_macro = BTreeMap::new();
    inside_macro.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"macro_rules! m { () => { include!(\"part.rs\"); }; }\n".to_vec(),
    );
    assert_eq!(
        scan_src_includes(&inside_macro)?.get("crates/example/src/lib.rs"),
        Some(&1)
    );

    // Raw identifier `r#include!` is also caught.
    let mut raw_ident = BTreeMap::new();
    raw_ident.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"r#include!(\"part.rs\");\n".to_vec(),
    );
    assert_eq!(
        scan_src_includes(&raw_ident)?.get("crates/example/src/lib.rs"),
        Some(&1)
    );
    Ok(())
}
