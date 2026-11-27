//! Shrink-only ban on `include!()` in production `src/`.
//!
//! Stitching Rust source together with `include!` hides real module boundaries
//! and gives parts no interface. Zero non-test `include!()` stitches are
//! permitted under `crates/*/src/`: every retained carrier in
//! `include-ratchet.json` must be genuinely gated by `#[cfg(test)]` (verified
//! via `syn` AST, never by filename alone), and the list may only shrink.
use super::*;
use syn::visit::{self, Visit};

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

/// Extract a `#[path = "..."]` string literal value from `attrs`, if present.
fn path_attr_value(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = &nv.value
            {
                return Some(lit_str.value());
            }
        }
    }
    None
}

/// Resolve a relative `#[path = "..."]` value against the directory of `source_file`.
fn resolve_relative_mod_path(source_file: &str, rel: &str) -> String {
    let base_dir = source_file
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("");
    let mut segments: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    let normalized = rel.replace('\\', "/");
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

struct ExternalModDeclVisitor<'a> {
    source_file: &'a str,
    carrier_path: &'a str,
    carrier_mod_name: &'a str,
    is_standard_parent: bool,
    in_cfg_test: bool,
    found_count: usize,
    has_ungated: bool,
}

impl<'ast> Visit<'ast> for ExternalModDeclVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let prev = self.in_cfg_test;
        if has_cfg_test_attr(&node.attrs) {
            self.in_cfg_test = true;
        }
        if node.content.is_none() {
            let loads_carrier = match path_attr_value(&node.attrs) {
                Some(rel) => resolve_relative_mod_path(self.source_file, &rel) == self.carrier_path,
                None => self.is_standard_parent && node.ident == self.carrier_mod_name,
            };
            if loads_carrier {
                self.found_count += 1;
                if !self.in_cfg_test {
                    self.has_ungated = true;
                }
            }
        } else {
            visit::visit_item_mod(self, node);
        }
        self.in_cfg_test = prev;
    }
}

/// Return `true` if `carrier_path` is an external submodule file whose every
/// `mod` declaration across production `src/` is gated by `#[cfg(test)]`
/// (and at least one such declaration exists).
fn is_parent_cfg_test_module(carrier_path: &str, files: &BTreeMap<String, Vec<u8>>) -> bool {
    let Some(stem) = carrier_path.strip_suffix(".rs") else {
        return false;
    };
    let Some((dir, mod_name)) = stem.rsplit_once('/') else {
        return false;
    };
    let standard_parents = [format!("{dir}.rs"), format!("{dir}/mod.rs")];
    let carrier_file_name = carrier_path.rsplit('/').next().unwrap_or(carrier_path);
    let mut found_count = 0usize;
    let mut has_ungated = false;

    for (source_file, bytes) in files {
        if !is_production_src(source_file) {
            continue;
        }
        let is_standard_parent = standard_parents.iter().any(|p| p == source_file);
        let Ok(src) = std::str::from_utf8(bytes) else {
            continue;
        };
        // Fast skip for non-parent files that cannot possibly name this carrier in #[path = "..."].
        if !is_standard_parent && !(src.contains("path") && src.contains(carrier_file_name)) {
            continue;
        }
        let Ok(ast) = syn::parse_file(src) else {
            continue;
        };
        let mut visitor = ExternalModDeclVisitor {
            source_file,
            carrier_path,
            carrier_mod_name: mod_name,
            is_standard_parent,
            in_cfg_test: false,
            found_count: 0,
            has_ungated: false,
        };
        visitor.visit_file(&ast);
        found_count += visitor.found_count;
        has_ungated |= visitor.has_ungated;
    }

    found_count >= 1 && !has_ungated
}

struct IncludeCfgTestVisitor<'a> {
    carrier: &'a str,
    in_cfg_test: bool,
    problems: Vec<String>,
}

impl IncludeCfgTestVisitor<'_> {
    fn visit_with_attrs<F>(&mut self, attrs: &[syn::Attribute], f: F)
    where
        F: FnOnce(&mut Self),
    {
        let prev = self.in_cfg_test;
        if has_cfg_test_attr(attrs) {
            self.in_cfg_test = true;
        }
        f(self);
        self.in_cfg_test = prev;
    }
}

impl<'ast> Visit<'ast> for IncludeCfgTestVisitor<'_> {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if is_include_macro(node) && !self.in_cfg_test {
            self.problems.push(format!(
                "{}: non-test include!() in production src/ is forbidden; gate with #[cfg(test)] or convert to a submodule",
                self.carrier
            ));
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_item_mod(v, node));
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_item_fn(v, node));
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_item_impl(v, node));
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_item_trait(v, node));
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_item_macro(v, node));
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_impl_item_fn(v, node));
    }

    fn visit_impl_item_macro(&mut self, node: &'ast syn::ImplItemMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_impl_item_macro(v, node));
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_trait_item_fn(v, node));
    }

    fn visit_trait_item_macro(&mut self, node: &'ast syn::TraitItemMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_trait_item_macro(v, node));
    }

    fn visit_foreign_item_macro(&mut self, node: &'ast syn::ForeignItemMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_foreign_item_macro(v, node));
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_stmt_macro(v, node));
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_expr_macro(v, node));
    }

    fn visit_expr_block(&mut self, node: &'ast syn::ExprBlock) {
        self.visit_with_attrs(&node.attrs, |v| visit::visit_expr_block(v, node));
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
    let mut visitor = IncludeCfgTestVisitor {
        carrier,
        in_cfg_test: false,
        problems: Vec::new(),
    };
    visitor.visit_file(&ast);
    Ok(visitor.problems)
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

    // An ungated include!() inside an ordinary function body must be caught.
    let mut fn_body = BTreeMap::new();
    fn_body.insert(
        "crates/example/src/fn_body.rs".to_owned(),
        b"fn prod() {\n    let _ = include!(\"part.rs\");\n}\n".to_vec(),
    );
    assert_eq!(
        verify_carrier_cfg_test_only("crates/example/src/fn_body.rs", &fn_body)?.len(),
        1
    );
    fn_body.insert(
        "crates/example/src/fn_body.rs".to_owned(),
        b"#[cfg(test)]\nfn test_only() {\n    let _ = include!(\"part.rs\");\n}\n".to_vec(),
    );
    assert!(verify_carrier_cfg_test_only("crates/example/src/fn_body.rs", &fn_body)?.is_empty());

    // A second non-test mod declaration loading the same external carrier fails.
    let mut duplicate_mod = BTreeMap::new();
    duplicate_mod.insert(
        "crates/example/src/write.rs".to_owned(),
        b"#[cfg(test)]\nmod tests;\nmod tests;\n".to_vec(),
    );
    duplicate_mod.insert(
        "crates/example/src/write/tests.rs".to_owned(),
        b"include!(\"tests/a.rs\");\n".to_vec(),
    );
    assert_eq!(
        verify_carrier_cfg_test_only("crates/example/src/write/tests.rs", &duplicate_mod)?.len(),
        1
    );

    // A non-test #[path = "..."] mod declaration from another file also fails.
    let mut path_alias = BTreeMap::new();
    path_alias.insert(
        "crates/example/src/write.rs".to_owned(),
        b"#[cfg(test)]\nmod tests;\n".to_vec(),
    );
    path_alias.insert(
        "crates/example/src/other.rs".to_owned(),
        b"#[path = \"write/tests.rs\"]\nmod leaked;\n".to_vec(),
    );
    path_alias.insert(
        "crates/example/src/write/tests.rs".to_owned(),
        b"include!(\"tests/a.rs\");\n".to_vec(),
    );
    assert_eq!(
        verify_carrier_cfg_test_only("crates/example/src/write/tests.rs", &path_alias)?.len(),
        1
    );
    Ok(())
}
