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

type FragmentCounts = BTreeMap<String, usize>;

fn listed(root: &Path) -> Result<BTreeMap<String, FragmentCounts>, String> {
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
    Ok(BTreeMap::new())
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
    let problems = evaluate_ratchet(&found, &allowed);
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
