//! AST and macro-token scanner for `#[expect(..., reason = "legacy")]`.

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{AttrStyle, Attribute, Expr, ExprLit, Lit, Meta, Token};

use super::LintCounts;

fn push_token(out: &mut String, prev_joint: &mut bool, token: &str, is_joint: bool) {
    if !out.is_empty() && !*prev_joint {
        out.push(' ');
    }
    out.push_str(token);
    *prev_joint = is_joint;
}

fn normalize_parse_stream(
    input: syn::parse::ParseStream<'_>,
    out: &mut String,
    prev_joint: &mut bool,
) -> syn::Result<()> {
    while !input.is_empty() {
        if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            push_token(out, prev_joint, "(", false);
            normalize_parse_stream(&content, out, prev_joint)?;
            push_token(out, prev_joint, ")", false);
        } else if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            push_token(out, prev_joint, "{", false);
            normalize_parse_stream(&content, out, prev_joint)?;
            push_token(out, prev_joint, "}", false);
        } else if input.peek(syn::token::Bracket) {
            let content;
            syn::bracketed!(content in input);
            push_token(out, prev_joint, "[", false);
            normalize_parse_stream(&content, out, prev_joint)?;
            push_token(out, prev_joint, "]", false);
        } else {
            // Preserve each leaf token's text verbatim (including internal
            // whitespace in string/byte/char literals) and distinguish Joint vs
            // Alone punctuation spacing (`..` vs `. .`, `+=` vs `+ =`).
            input.step(|cursor| {
                if let Some((punct, next)) = cursor.punct() {
                    let joint = format!("{:?}", punct.spacing()) == "Joint";
                    push_token(out, prev_joint, &punct.to_string(), joint);
                    Ok(((), next))
                } else if let Some((tree, next)) = cursor.token_tree() {
                    push_token(out, prev_joint, &tree.to_string(), false);
                    Ok(((), next))
                } else {
                    Err(cursor.error("unexpected end of token stream"))
                }
            })?;
        }
    }
    Ok(())
}

fn normalize_tokens(tokens: &impl ToTokens) -> String {
    let mut out = String::new();
    let mut prev_joint = false;
    let _ = syn::parse::Parser::parse2(
        |input: syn::parse::ParseStream<'_>| {
            normalize_parse_stream(input, &mut out, &mut prev_joint)
        },
        tokens.to_token_stream(),
    );
    out
}

fn compact_lint_meta(tokens: &impl ToTokens) -> String {
    tokens
        .to_token_stream()
        .to_string()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn is_reason_ident(path: &syn::Path) -> bool {
    path.get_ident().is_some_and(|ident| {
        let text = ident.to_string();
        text == "reason" || text == "r#reason"
    })
}

fn is_ident_named(path: &syn::Path, name: &str) -> bool {
    path.get_ident().is_some_and(|ident| {
        let text = ident.to_string();
        text == name || text.strip_prefix("r#") == Some(name)
    })
}

fn extract_expect_lints(list: &syn::MetaList) -> Result<Vec<String>, String> {
    let nested = list
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .map_err(|error| format!("invalid #[expect(...)] arguments: {error}"))?;
    let mut lints = Vec::new();
    let mut is_legacy = false;
    for meta in nested {
        if let Meta::NameValue(name_value) = &meta {
            if is_reason_ident(&name_value.path) {
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(lit), ..
                }) = &name_value.value
                {
                    if lit.value() == "legacy" {
                        is_legacy = true;
                    }
                }
                continue;
            }
        }
        let lint = compact_lint_meta(&meta);
        if !lint.is_empty() {
            lints.push(lint);
        }
    }
    if is_legacy {
        if lints.is_empty() {
            lints.push("<unknown>".to_owned());
        }
        Ok(lints)
    } else {
        Ok(Vec::new())
    }
}

fn collect_legacy_expects_in_meta(meta: &Meta) -> Result<Vec<String>, String> {
    let Meta::List(list) = meta else {
        return Ok(Vec::new());
    };
    if is_ident_named(&list.path, "expect") {
        return extract_expect_lints(list);
    }
    if is_ident_named(&list.path, "cfg_attr") {
        let nested = list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .map_err(|error| format!("invalid #[cfg_attr(...)] arguments: {error}"))?;
        let mut found = Vec::new();
        for item in nested.iter().skip(1) {
            found.extend(collect_legacy_expects_in_meta(item)?);
        }
        return Ok(found);
    }
    Ok(Vec::new())
}

fn impl_scope_label(node: &syn::ItemImpl) -> String {
    let self_ty = normalize_tokens(&node.self_ty);
    if let Some((_, trait_path, _)) = &node.trait_ {
        format!("impl {} for {self_ty}", normalize_tokens(trait_path))
    } else {
        format!("impl {self_ty}")
    }
}

fn scoped_item_key(scopes: &[String], item: &str) -> String {
    if scopes.is_empty() {
        return item.to_owned();
    }
    let mut out = String::new();
    for scope in scopes {
        out.push_str(scope);
        out.push_str("::");
    }
    out.push_str(item);
    out
}

struct LegacyExpectVisitor {
    scopes: Vec<String>,
    counts: LintCounts,
    error: Option<String>,
}

impl LegacyExpectVisitor {
    fn new() -> Self {
        Self {
            scopes: Vec::new(),
            counts: LintCounts::new(),
            error: None,
        }
    }

    fn record_attr(&mut self, attr: &Attribute) {
        if self.error.is_some() {
            return;
        }
        let lints = match collect_legacy_expects_in_meta(&attr.meta) {
            Ok(lints) => lints,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        if lints.is_empty() {
            return;
        }
        let scoped = if self.scopes.is_empty() {
            "#![file]".to_owned()
        } else if matches!(attr.style, AttrStyle::Inner(_)) {
            scoped_item_key(&self.scopes, "#![file]")
        } else {
            self.scopes.join("::")
        };
        for lint in lints {
            *self
                .counts
                .entry(format!("{scoped} :: {lint}"))
                .or_insert(0) += 1;
        }
    }

    fn scan_macro_tokens(&mut self, mac: &syn::Macro) {
        let _ = syn::parse::Parser::parse2(
            |input: syn::parse::ParseStream<'_>| scan_macro_stream(input, self),
            mac.tokens.clone(),
        );
    }
}

fn scan_macro_stream(
    input: syn::parse::ParseStream<'_>,
    visitor: &mut LegacyExpectVisitor,
) -> syn::Result<()> {
    while !input.is_empty() {
        // Consume an entire adjacent attribute run directly from `input` once
        // so `#[inline] #[expect(..., reason = "legacy")]` is never counted twice.
        if input.peek(Token![#]) {
            let fork = input.fork();
            if fork
                .call(Attribute::parse_outer)
                .is_ok_and(|attrs| !attrs.is_empty())
            {
                if let Ok(attrs) = input.call(Attribute::parse_outer) {
                    for attr in &attrs {
                        visitor.visit_attribute(attr);
                    }
                    continue;
                }
            } else {
                let fork_inner = input.fork();
                if fork_inner
                    .call(Attribute::parse_inner)
                    .is_ok_and(|attrs| !attrs.is_empty())
                {
                    if let Ok(attrs) = input.call(Attribute::parse_inner) {
                        for attr in &attrs {
                            visitor.visit_attribute(attr);
                        }
                        continue;
                    }
                }
            }
        }
        if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            scan_macro_stream(&content, visitor)?;
        } else if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            scan_macro_stream(&content, visitor)?;
        } else if input.peek(syn::token::Bracket) {
            let content;
            syn::bracketed!(content in input);
            scan_macro_stream(&content, visitor)?;
        } else {
            let stepped = input.step(|cursor| {
                if let Some((_, next)) = cursor.token_tree() {
                    Ok(((), next))
                } else {
                    Err(cursor.error("unexpected end of macro stream"))
                }
            });
            if stepped.is_err() {
                break;
            }
        }
    }
    Ok(())
}

impl<'ast> Visit<'ast> for LegacyExpectVisitor {
    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        self.record_attr(attr);
        visit::visit_attribute(self, attr);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.scan_macro_tokens(node);
        visit::visit_macro(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.scopes.push(format!("mod {}", node.ident));
        visit::visit_item_mod(self, node);
        self.scopes.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.scopes.push(format!("trait {}", node.ident));
        visit::visit_item_trait(self, node);
        self.scopes.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.scopes.push(impl_scope_label(node));
        visit::visit_item_impl(self, node);
        self.scopes.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.scopes.push(format!("fn {}", node.sig.ident));
        visit::visit_item_fn(self, node);
        self.scopes.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.scopes.push(format!("fn {}", node.sig.ident));
        visit::visit_impl_item_fn(self, node);
        self.scopes.pop();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.scopes.push(format!("fn {}", node.sig.ident));
        visit::visit_trait_item_fn(self, node);
        self.scopes.pop();
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.scopes.push(format!("struct {}", node.ident));
        visit::visit_item_struct(self, node);
        self.scopes.pop();
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.scopes.push(format!("enum {}", node.ident));
        visit::visit_item_enum(self, node);
        self.scopes.pop();
    }

    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        self.scopes.push(format!("variant {}", node.ident));
        visit::visit_variant(self, node);
        self.scopes.pop();
    }

    fn visit_fields_named(&mut self, node: &'ast syn::FieldsNamed) {
        for field in &node.named {
            let label = field
                .ident
                .as_ref()
                .map_or_else(|| "field".to_owned(), |id| format!("field {id}"));
            self.scopes.push(label);
            visit::visit_field(self, field);
            self.scopes.pop();
        }
    }

    fn visit_fields_unnamed(&mut self, node: &'ast syn::FieldsUnnamed) {
        for (index, field) in node.unnamed.iter().enumerate() {
            self.scopes.push(format!("field {index}"));
            visit::visit_field(self, field);
            self.scopes.pop();
        }
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.scopes.push(format!("union {}", node.ident));
        visit::visit_item_union(self, node);
        self.scopes.pop();
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.scopes.push(format!("const {}", node.ident));
        visit::visit_item_const(self, node);
        self.scopes.pop();
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.scopes.push(format!("static {}", node.ident));
        visit::visit_item_static(self, node);
        self.scopes.pop();
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.scopes.push(format!("type {}", node.ident));
        visit::visit_item_type(self, node);
        self.scopes.pop();
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        self.scopes.push(format!("const {}", node.ident));
        visit::visit_impl_item_const(self, node);
        self.scopes.pop();
    }

    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        self.scopes.push(format!("type {}", node.ident));
        visit::visit_impl_item_type(self, node);
        self.scopes.pop();
    }

    fn visit_trait_item_const(&mut self, node: &'ast syn::TraitItemConst) {
        self.scopes.push(format!("const {}", node.ident));
        visit::visit_trait_item_const(self, node);
        self.scopes.pop();
    }

    fn visit_trait_item_type(&mut self, node: &'ast syn::TraitItemType) {
        self.scopes.push(format!("type {}", node.ident));
        visit::visit_trait_item_type(self, node);
        self.scopes.pop();
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.scopes.push("use".to_owned());
        visit::visit_item_use(self, node);
        self.scopes.pop();
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        let label = node
            .ident
            .as_ref()
            .map_or_else(|| "macro".to_owned(), |id| format!("macro {id}"));
        self.scopes.push(label);
        visit::visit_item_macro(self, node);
        self.scopes.pop();
    }
}

fn parse_rust_source(source: &str) -> Result<syn::File, String> {
    match syn::parse_file(source) {
        Ok(file) => Ok(file),
        Err(file_err) => {
            // Included test/body fragments may contain bare statements or items.
            let wrapped = format!("fn __fragment__() {{\n{source}\n}}");
            syn::parse_file(&wrapped)
                .map_err(|_| format!("failed to parse Rust source: {file_err}"))
        }
    }
}

pub(super) fn scan_legacy_expects(bytes: &[u8]) -> Result<LintCounts, String> {
    let source = std::str::from_utf8(bytes).map_err(|error| format!("invalid UTF-8: {error}"))?;
    let file = parse_rust_source(source)?;
    let mut visitor = LegacyExpectVisitor::new();
    visitor.visit_file(&file);
    if let Some(error) = visitor.error {
        return Err(error);
    }
    Ok(visitor.counts)
}
