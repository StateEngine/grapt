//! Shrink-only ban on `include!()` in production `src/`.
//!
//! Stitching Rust source together with `include!` hides real module boundaries
//! and gives parts no interface. Zero non-test `include!()` stitches are
//! permitted under `crates/*/src/`: every retained carrier in
//! `include-ratchet.json` must be genuinely gated by `#[cfg(test)]` (verified
//! via `syn` AST, never by filename alone), and the list may only shrink.
use super::*;

const RATCHET: &str = "crates/grapt-types/tests/include-ratchet.json";

fn is_production_src(path: &str) -> bool {
    let mut parts = path.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("crates"), Some(_), Some("src"), Some(_))
    )
}

fn is_cfg_test_meta(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => {
            let mut found_test = false;
            let _ = list.parse_nested_meta(|nested| {
                if nested.path.is_ident("test") {
                    found_test = true;
                }
                Ok(())
            });
            found_test
        }
        _ => false,
    }
}

fn has_cfg_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        attr.parse_args::<syn::Meta>()
            .map(|meta| is_cfg_test_meta(&meta))
            .unwrap_or(false)
    })
}

fn is_include_macro(mac: &syn::Macro) -> bool {
    mac.path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == "include")
}

/// Return `true` if `carrier_path` is an external submodule file whose parent
/// file declares it with `#[cfg(test)]` (e.g. `#[cfg(test)] mod tests;`).
fn is_parent_cfg_test_module(carrier_path: &str, files: &BTreeMap<String, Vec<u8>>) -> bool {
    let Some(stem) = carrier_path.strip_suffix(".rs") else {
        return false;
    };
    let Some((dir, mod_name)) = stem.rsplit_once('/') else {
        return false;
    };
    let candidates = [format!("{dir}.rs"), format!("{dir}/mod.rs")];
    for parent in candidates {
        let Some(bytes) = files.get(&parent) else {
            continue;
        };
        let Ok(src) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(ast) = syn::parse_file(src) else {
            continue;
        };
        if declares_cfg_test_external_mod(&ast.items, mod_name) {
            return true;
        }
    }
    false
}

fn declares_cfg_test_external_mod(items: &[syn::Item], mod_name: &str) -> bool {
    for item in items {
        if let syn::Item::Mod(item_mod) = item {
            if item_mod.content.is_none()
                && item_mod.ident == mod_name
                && has_cfg_test_attr(&item_mod.attrs)
            {
                return true;
            }
            if let Some((_, inner)) = &item_mod.content {
                if has_cfg_test_attr(&item_mod.attrs) && declares_external_mod_any(inner, mod_name)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn declares_external_mod_any(items: &[syn::Item], mod_name: &str) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            syn::Item::Mod(item_mod) if item_mod.content.is_none() && item_mod.ident == mod_name
        )
    })
}

fn verify_items_cfg_test_gated(
    carrier: &str,
    items: &[syn::Item],
    in_cfg_test: bool,
    problems: &mut Vec<String>,
) {
    for item in items {
        match item {
            syn::Item::Macro(item_mac) if is_include_macro(&item_mac.mac) => {
                if !in_cfg_test && !has_cfg_test_attr(&item_mac.attrs) {
                    problems.push(format!(
                        "{carrier}: non-test include!() in production src/ is forbidden; gate with #[cfg(test)] or convert to a submodule"
                    ));
                }
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, inner)) = &item_mod.content {
                    let gated = in_cfg_test || has_cfg_test_attr(&item_mod.attrs);
                    verify_items_cfg_test_gated(carrier, inner, gated, problems);
                }
            }
            _ => {}
        }
    }
}

fn verify_carrier_cfg_test_only(
    carrier: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<String>, String> {
    if is_parent_cfg_test_module(carrier, files) {
        return Ok(Vec::new());
    }
    let Some(bytes) = files.get(carrier) else {
        return Err(format!("{carrier}: missing from file inventory"));
    };
    let src = std::str::from_utf8(bytes).map_err(|_| format!("{carrier}: invalid UTF-8"))?;
    let ast = syn::parse_file(src).map_err(|error| format!("{carrier}: syn parse: {error}"))?;
    let mut problems = Vec::new();
    verify_items_cfg_test_gated(carrier, &ast.items, false, &mut problems);
    Ok(problems)
}

type FragmentCounts = BTreeMap<String, usize>;

fn listed(root: &Path) -> Result<BTreeMap<String, FragmentCounts>, String> {
    let bytes = read_bounded(&root.join(RATCHET), MAX_FILE_BYTES)?;
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{RATCHET}: {error}"))?;
    let carriers = document["carriers"]
        .as_object()
        .ok_or_else(|| format!("{RATCHET}: carriers must be an object"))?;
    let mut out = BTreeMap::new();
    for (carrier, entry) in carriers {
        if !is_production_src(carrier) {
            return Err(format!(
                "{RATCHET}: {carrier} is outside production src/; remove it"
            ));
        }
        let mut counts = FragmentCounts::new();
        for fragment in entry
            .as_array()
            .ok_or_else(|| format!("{RATCHET}: {carrier} must be a fragment array"))?
        {
            let name = fragment
                .as_str()
                .ok_or_else(|| format!("{RATCHET}: {carrier} has a non-string fragment"))?;
            *counts.entry(name.to_owned()).or_insert(0) += 1;
        }
        if counts.is_empty() {
            return Err(format!(
                "{RATCHET}: {carrier} has no fragments; delete the empty entry"
            ));
        }
        out.insert(carrier.clone(), counts);
    }
    Ok(out)
}

/// Every production `src/` carrier and its stitched fragment occurrence counts.
fn stitched_src(
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, FragmentCounts>, String> {
    let mut out: BTreeMap<String, FragmentCounts> = BTreeMap::new();
    for (path, bytes) in files {
        if !is_production_src(path) {
            continue;
        }
        for site in detect_sites(path, bytes).map_err(|error| format!("{path}: {error}"))? {
            if !files.contains_key(&site.target) {
                return Err(format!("{path} includes missing {}", site.target));
            }
            *out.entry(path.clone())
                .or_default()
                .entry(site.target)
                .or_insert(0) += 1;
        }
    }
    Ok(out)
}

fn evaluate_ratchet(
    found: &BTreeMap<String, FragmentCounts>,
    allowed: &BTreeMap<String, FragmentCounts>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (carrier, fragments) in found {
        let Some(limit) = allowed.get(carrier) else {
            problems.push(format!(
                "{carrier}: new include! in production src/; use a submodule with a real interface"
            ));
            continue;
        };
        for (fragment, count) in fragments {
            let allowed_count = limit.get(fragment).copied().unwrap_or(0);
            if *count > allowed_count {
                problems.push(format!(
                    "{carrier}: new include!(\"{fragment}\") in production src/ ({count} > {allowed_count}); use a submodule with a real interface"
                ));
            }
        }
    }
    for (carrier, limit) in allowed {
        let now = found.get(carrier);
        for (fragment, allowed_count) in limit {
            let now_count = now
                .and_then(|counts| counts.get(fragment))
                .copied()
                .unwrap_or(0);
            if now_count == 0 {
                problems.push(format!(
                    "{carrier}: {fragment} is no longer stitched; delete it from {RATCHET}"
                ));
            } else if now_count < *allowed_count {
                problems.push(format!(
                    "{carrier}: {fragment} occurrences shrank from {allowed_count} to {now_count}; update {RATCHET}"
                ));
            }
        }
    }
    problems
}

#[test]
fn production_src_include_sites_only_shrink() -> Result<(), Box<dyn Error>> {
    let _tree = lock_tree();
    let root = workspace_root();
    let inventory = inventory(&root)?;
    let allowed = listed(&root)?;
    let found = stitched_src(&inventory.files)?;
    let mut problems = evaluate_ratchet(&found, &allowed);
    for carrier in found.keys() {
        problems.extend(verify_carrier_cfg_test_only(carrier, &inventory.files)?);
    }
    if problems.is_empty() {
        return Ok(());
    }
    Err(bounded_diagnostic(
        "include! ratchet: ",
        &problems.iter().map(String::as_str).collect::<Vec<_>>(),
    )?
    .into())
}

#[test]
fn ratchet_tracks_only_production_src_and_refuses_growth_or_stale_entries(
) -> Result<(), Box<dyn Error>> {
    let mut files = BTreeMap::new();
    files.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"include!(\"part.rs\");\n".to_vec(),
    );
    files.insert(
        "crates/example/src/part.rs".to_owned(),
        b"fn a() {}\n".to_vec(),
    );
    files.insert(
        "crates/example/tests/suite.rs".to_owned(),
        b"include!(\"helper.rs\");\n".to_vec(),
    );
    files.insert(
        "crates/example/tests/helper.rs".to_owned(),
        b"fn helper() {}\n".to_vec(),
    );

    let found = stitched_src(&files)?;
    assert_eq!(found.len(), 1);
    assert!(found.contains_key("crates/example/src/lib.rs"));

    let empty = BTreeMap::new();
    let added = evaluate_ratchet(&found, &empty);
    assert_eq!(added.len(), 1);
    assert!(added[0].contains("new include! in production src/"));

    let stale = evaluate_ratchet(&empty, &found);
    assert_eq!(stale.len(), 1);
    assert!(stale[0].contains("no longer stitched"));

    let mut duplicate_files = files.clone();
    duplicate_files.insert(
        "crates/example/src/lib.rs".to_owned(),
        b"include!(\"part.rs\");\ninclude!(\"part.rs\");\n".to_vec(),
    );
    let duplicate_found = stitched_src(&duplicate_files)?;
    let duplicate_problems = evaluate_ratchet(&duplicate_found, &found);
    assert_eq!(duplicate_problems.len(), 1);
    assert!(duplicate_problems[0].contains("new include!"));
    assert!(duplicate_problems[0].contains("2 > 1"));
    Ok(())
}

#[test]
fn ast_gate_requires_cfg_test_on_every_src_include() -> Result<(), Box<dyn Error>> {
    // Inline #[cfg(test)] mod tests { include!(...); } is admitted.
    let mut gated = BTreeMap::new();
    gated.insert(
        "crates/example/src/context.rs".to_owned(),
        b"#[cfg(test)]\nmod tests {\n    include!(\"context/tests.rs\");\n}\n".to_vec(),
    );
    assert!(verify_carrier_cfg_test_only("crates/example/src/context.rs", &gated)?.is_empty());

    // Removing #[cfg(test)] from context.rs is refused even though the filename is tests.rs!
    let mut ungated = BTreeMap::new();
    ungated.insert(
        "crates/example/src/context.rs".to_owned(),
        b"mod tests {\n    include!(\"context/tests.rs\");\n}\n".to_vec(),
    );
    let problems = verify_carrier_cfg_test_only("crates/example/src/context.rs", &ungated)?;
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("non-test include!()"));

    // #[cfg(not(test))] is also refused.
    let mut inverted = BTreeMap::new();
    inverted.insert(
        "crates/example/src/context.rs".to_owned(),
        b"#[cfg(not(test))]\nmod tests {\n    include!(\"context/tests.rs\");\n}\n".to_vec(),
    );
    assert_eq!(
        verify_carrier_cfg_test_only("crates/example/src/context.rs", &inverted)?.len(),
        1
    );

    // External tests.rs file is admitted ONLY when parent declares #[cfg(test)] mod tests;
    let mut external = BTreeMap::new();
    external.insert(
        "crates/example/src/write.rs".to_owned(),
        b"#[cfg(test)]\nmod tests;\n".to_vec(),
    );
    external.insert(
        "crates/example/src/write/tests.rs".to_owned(),
        b"include!(\"tests/a.rs\");\n".to_vec(),
    );
    assert!(
        verify_carrier_cfg_test_only("crates/example/src/write/tests.rs", &external)?.is_empty()
    );

    external.insert(
        "crates/example/src/write.rs".to_owned(),
        b"mod tests;\n".to_vec(),
    );
    assert_eq!(
        verify_carrier_cfg_test_only("crates/example/src/write/tests.rs", &external)?.len(),
        1
    );
    Ok(())
}
