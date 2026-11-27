// ---------------------------------------------------------------------------
// The forbidden set
// ---------------------------------------------------------------------------

/// Class A — tokens with no lawful reason to appear in Rust source.
///
/// Whitespace is stripped from both sides before comparison, so a reimplementation
/// cannot hide behind formatting: `(string & {})` and `(string&{})` are the same
/// anchor. Each entry names the §3.3 pass it belongs to, because a row that
/// cannot say what it is guarding is not a guard.
pub(super) const LITERAL_ANCHORS: &[(&str, &str)] = &[
    ("Brand.Brand<", "pass 1: brand erasure (index.ts:1014)"),
    (
        "Schema.Json",
        "pass 2: Schema.Json alias, source side (index.ts:1015)",
    ),
    (
        "JsonValue",
        "pass 2: Schema.Json alias, target side (index.ts:1015)",
    ),
    (
        "(string & {})",
        "pass 4: preserveStringSuggestions (index.ts:1064)",
    ),
    (
        "ReadonlyArray<",
        "pass 5: mutableType array rewrite (index.ts:852)",
    ),
];

/// Class B — payload rules judged over the rewrite-construction spans of a file.
///
/// `Pair` fires when both payloads are present anywhere in the file's rewrite
/// spans; `Single` fires on one. Both are inert outside a rewrite span, which is
/// the entire reason this class exists separately from class A.
#[derive(Debug)]
pub(super) enum PayloadRule {
    /// A payload *containing* the first token, and a payload *equal to* the second.
    Pair(&'static str, &'static str, &'static str),
    /// A payload containing this token.
    Single(&'static str, &'static str),
}

pub(super) const PAYLOAD_RULES: &[PayloadRule] = &[
    PayloadRule::Pair(
        "unknown",
        "any",
        "pass 3: unknown->any widening (index.ts:1016)",
    ),
    PayloadRule::Single(
        "readonly",
        "pass 5: mutableType readonly strip (index.ts:852)",
    ),
];

/// How a call name's LEFT edge is matched, which differs by token family and is
/// not a detail.
///
/// R2 applied one identifier boundary to every token and the R2 review (DC-2)
/// showed that was two mistakes at once: the boundary was uncarried by any test,
/// and it silently *narrowed* the `Regex::new` family, which R1 had deliberately
/// matched by suffix. The policy is therefore stated per family, and both halves
/// carry an executable control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftEdge {
    /// The name must not continue an identifier: `t.replace(`, `str::replace(`
    /// and a bare `replace(` are calls to it; `send_replace(` and
    /// `str_replace(` are different functions and must not open a span.
    ///
    /// This matters beyond tidiness because payloads are POOLED per file: a
    /// spurious span widens the pool the cross-span pair rule is judged over, so
    /// it is a false-positive surface. Carried by the `send_replace` pair in
    /// `lawful_rust_stays_green` and attributed by `QC12`.
    Bare,
    /// Any prefix is admitted — deliberate SUFFIX coverage, so `Regex::new`,
    /// `regex::Regex::new`, `regex_lite::Regex::new` and `LazyRegex::new` are
    /// all the same constructor.
    ///
    /// These are type-qualified names rather than bare identifiers, so the
    /// wrapper-name false positive that justifies `Bare` cannot arise: nothing
    /// lawful is called `…Regex::new` without being a regex constructor.
    /// Carried by the `LazyRegex::new` case in
    /// `the_widening_is_detected_in_every_fully_qualified_spelling`.
    Suffix,
}

/// Calls that construct or perform a textual rewrite, and how each is matched.
///
/// Matched by NAME, never by receiver syntax, and never with the opening paren
/// welded on — see `find_call_sites`. Both of those were survivors:
///
/// * a leading `.` matched the method spelling only and missed
///   `str::replace(t, "unknown", "any")` (R1 EP-C1);
/// * welding `(` to the name missed `t.replace::<&str>("unknown", "any")` and
///   `str::replace::<&str>(t, …)` (R2 DC-1).
///
/// Both compile, hide nothing, and perform pass 3 — the one ruled pass with no
/// class A anchor to fall back on.
///
/// `Regex::new` is here because the widening can be split across two calls, with
/// the pattern built in one and applied in another.
const REWRITE_CALLS: &[(&str, LeftEdge)] = &[
    ("replace", LeftEdge::Bare),
    ("replacen", LeftEdge::Bare),
    ("replace_all", LeftEdge::Bare),
    ("replacen_all", LeftEdge::Bare),
    ("Regex::new", LeftEdge::Suffix),
    ("RegexBuilder::new", LeftEdge::Suffix),
];

// ---------------------------------------------------------------------------
// Source normalisation
// ---------------------------------------------------------------------------

/// One source file reduced to whitespace-free code, with provenance kept per
/// character.
#[derive(Debug)]
struct Normalized {
    /// Code with every comment removed and every whitespace character dropped.
    text: String,
    /// Source line (1-based) of each character in `text`.
    line: Vec<usize>,
    /// Whether each character in `text` came from inside a string or char literal.
    in_literal: Vec<bool>,
}

/// Strip comments, drop whitespace, and remember where every surviving character
/// came from.
///
/// Comments are dropped rather than skipped line-wise. The precedent gates skip a
/// line whose trimmed form starts with `//`, which is correct for them; here it
/// would be a hole, because a rewrite with a trailing comment would vanish from
/// the scan along with the comment. A character-level scanner has no such
/// asymmetry: a trailing comment removes the comment and keeps the code.
///
/// String and char literals are preserved verbatim (whitespace included) because
/// they carry the payloads class B judges.
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn normalize(source: &str) -> Normalized {
    let chars: Vec<char> = source.chars().collect();
    let mut text = String::new();
    let mut line_of = Vec::new();
    let mut in_literal = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;

    // Emit one character, recording its line and literal-ness.
    macro_rules! emit {
        ($ch:expr, $lit:expr) => {{
            text.push($ch);
            line_of.push(line);
            in_literal.push($lit);
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }

        // Line comment.
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Block comment, which nests in Rust.
        if c == '/' && next == Some('*') {
            let mut depth = 1usize;
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '\n' {
                    line += 1;
                    i += 1;
                } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Raw string: r"...", r#"..."#, and the byte forms br"..." / br#"..."#.
        if c == 'r' || (c == 'b' && next == Some('r')) {
            let start = if c == 'b' { i + 1 } else { i };
            let mut j = start + 1;
            let mut hashes = 0usize;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                // Confirmed a raw string; consume through its terminator.
                j += 1;
                loop {
                    match chars.get(j) {
                        None => break,
                        Some('\n') => {
                            line += 1;
                            emit!(' ', true);
                            j += 1;
                        }
                        Some('"') => {
                            let closed = (1..=hashes).all(|k| chars.get(j + k) == Some(&'#'));
                            if closed {
                                j += hashes + 1;
                                break;
                            }
                            emit!('"', true);
                            j += 1;
                        }
                        Some(&ch) => {
                            if !ch.is_whitespace() {
                                emit!(ch, true);
                            }
                            j += 1;
                        }
                    }
                }
                i = j;
                continue;
            }
        }

        // Ordinary string, including the byte form b"...".
        if c == '"' || (c == 'b' && next == Some('"')) {
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            loop {
                match chars.get(j) {
                    None => break,
                    Some('\\') => {
                        // An escape consumes the next character whatever it is,
                        // so a `\"` cannot end the literal early.
                        if chars.get(j + 1) == Some(&'\n') {
                            line += 1;
                        }
                        if let Some(&escaped) = chars.get(j + 1) {
                            if !escaped.is_whitespace() {
                                emit!(escaped, true);
                            }
                        }
                        j += 2;
                    }
                    Some('"') => {
                        j += 1;
                        break;
                    }
                    Some('\n') => {
                        line += 1;
                        j += 1;
                    }
                    Some(&ch) => {
                        if !ch.is_whitespace() {
                            emit!(ch, true);
                        }
                        j += 1;
                    }
                }
            }
            i = j;
            continue;
        }

        // Char literal, distinguished from a lifetime. `'a'` and `'\u{e9}'` are
        // literals; `'a` in `&'a str` is a lifetime and stays code.
        if c == '\'' {
            let is_escape = next == Some('\\');
            let is_simple = chars.get(i + 2) == Some(&'\'');
            if is_escape || is_simple {
                let mut j = i + 1;
                loop {
                    match chars.get(j) {
                        None => break,
                        Some('\\') => {
                            if let Some(&escaped) = chars.get(j + 1) {
                                if !escaped.is_whitespace() {
                                    emit!(escaped, true);
                                }
                            }
                            j += 2;
                        }
                        Some('\'') => {
                            j += 1;
                            break;
                        }
                        Some(&ch) => {
                            if !ch.is_whitespace() {
                                emit!(ch, true);
                            }
                            j += 1;
                        }
                    }
                }
                i = j;
                continue;
            }
        }

        if !c.is_whitespace() {
            emit!(c, false);
        }
        i += 1;
    }

    Normalized {
        text,
        line: line_of,
        in_literal,
    }
}

impl Normalized {
    /// Byte-offset -> source line. Offsets here are character indices, which is
    /// what every search in this file produces.
    fn line_at(&self, index: usize) -> usize {
        self.line.get(index).copied().unwrap_or(0)
    }

    /// Every occurrence of `needle` in the normalised code, as character indices.
    fn find_all(&self, needle: &str) -> Vec<usize> {
        let haystack: Vec<char> = self.text.chars().collect();
        let pattern: Vec<char> = needle.chars().collect();
        let mut hits = Vec::new();
        if pattern.is_empty() || pattern.len() > haystack.len() {
            return hits;
        }
        for start in 0..=(haystack.len() - pattern.len()) {
            if haystack[start..start + pattern.len()] == pattern[..] {
                hits.push(start);
            }
        }
        hits
    }

    /// Occurrences of `needle` that are real code, not text inside a literal.
    fn find_all_in_code(&self, needle: &str) -> Vec<usize> {
        self.find_all(needle)
            .into_iter()
            .filter(|start| !self.in_literal.get(*start).copied().unwrap_or(false))
            .collect()
    }

    /// Call sites of a rewrite function, by NAME rather than by receiver syntax.
    ///
    /// R1 EP-C1: matching `.replace(` matches the *method* spelling only, so
    /// `str::replace(t, "unknown", "any")` — ordinary compiling Rust that
    /// performs pass 3 with both literals verbatim in the argument list — was
    /// never collected and the sole carrier for pass 3 missed it. Matching the
    /// name covers the method form, the fully-qualified forms
    /// (`str::replace`, `String::replace`, `Regex::replace_all`) and a bare
    /// call alike.
    ///
    /// The left edge is matched per token family — see [`LeftEdge`].
    ///
    /// R2 DC-1: the name alone is not the whole call. A turbofish may sit
    /// legally between the name and its argument list, and
    /// `t.replace::<&str>("unknown", "any")` compiles and performs pass 3 with
    /// both literals verbatim inside the call's own argument list. Welding `(`
    /// onto the name missed it. So an optional `::<…>` is skipped by angle
    /// depth over CODE characters — a `<` or `>` inside a string literal must
    /// not move the depth — and only then is `(` required.
    ///
    /// Returns the index of that opening paren, which is where the span starts.
    fn find_call_sites(&self, name: &str, edge: LeftEdge) -> Vec<usize> {
        let chars: Vec<char> = self.text.chars().collect();
        let width = name.chars().count();
        let mut sites = Vec::new();
        for start in self.find_all_in_code(name) {
            if edge == LeftEdge::Bare {
                if let Some(before) = start.checked_sub(1).and_then(|at| chars.get(at)) {
                    if before.is_ascii_alphanumeric() || *before == '_' {
                        continue;
                    }
                }
            }
            let mut at = start + width;
            // An optional turbofish: `::<` … balanced … `>`.
            if chars.get(at) == Some(&':')
                && chars.get(at + 1) == Some(&':')
                && chars.get(at + 2) == Some(&'<')
            {
                let mut depth = 0usize;
                let mut scan = at + 2;
                while scan < chars.len() {
                    if self.in_literal.get(scan).copied().unwrap_or(false) {
                        scan += 1;
                        continue;
                    }
                    match chars[scan] {
                        '<' => depth += 1,
                        '>' if depth > 0 => {
                            depth -= 1;
                            if depth == 0 {
                                scan += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    scan += 1;
                }
                // An unbalanced turbofish is not a call; leaving `at` where it
                // was would make the paren check reject it anyway, but skipping
                // to the scan point keeps a balanced one exact.
                if depth == 0 {
                    at = scan;
                }
            }
            // The right edge. This is also what keeps `replace` from matching
            // inside `replace_all` and `replacen`: the character after the name
            // is `_` or `n` there, not `(` and not a turbofish.
            if chars.get(at) == Some(&'(') {
                sites.push(at);
            }
        }
        sites
    }

    /// The string-literal payloads inside `[from, to)`.
    ///
    /// A payload is a maximal run of literal characters, so
    /// `.replace("unknown", "any")` yields exactly `["unknown", "any"]`.
    fn payloads(&self, from: usize, to: usize) -> Vec<String> {
        let chars: Vec<char> = self.text.chars().collect();
        let mut out = Vec::new();
        let mut current = String::new();
        let end = to.min(chars.len());
        let start = from.min(end);
        for (offset, ch) in chars[start..end].iter().enumerate() {
            if self
                .in_literal
                .get(start + offset)
                .copied()
                .unwrap_or(false)
            {
                current.push(*ch);
            } else if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }

    /// The span of a call whose opening paren is the last character of a matched
    /// call token, found by counting parens over CODE characters only, so a paren
    /// inside a string literal cannot unbalance it.
    fn call_span(&self, open_paren: usize) -> (usize, usize) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut depth = 0usize;
        let mut index = open_paren;
        while index < chars.len() {
            let literal = self.in_literal.get(index).copied().unwrap_or(false);
            if !literal {
                if chars[index] == '(' {
                    depth += 1;
                } else if chars[index] == ')' && depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        return (open_paren, index + 1);
                    }
                }
            }
            index += 1;
        }
        // Unbalanced source: take the remainder rather than silently returning an
        // empty span, so a malformed file cannot hide a payload.
        (open_paren, chars.len())
    }
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(super) struct Findings {
    pub(super) violations: Vec<String>,
    pub(super) refusals: Vec<String>,
}

impl Findings {
    pub(super) fn is_clean(&self) -> bool {
        self.violations.is_empty() && self.refusals.is_empty()
    }
}

/// Judge one source file. `label` is what a violation will name.
pub(super) fn scan_source(label: &str, source: &str, found: &mut Findings) {
    let normalized = normalize(source);

    for (anchor, pass) in LITERAL_ANCHORS {
        let squeezed: String = anchor.chars().filter(|c| !c.is_whitespace()).collect();
        for at in normalized.find_all(&squeezed) {
            found.violations.push(format!(
                "{label}:{}: forbidden generator token `{anchor}` ({pass})",
                normalized.line_at(at)
            ));
        }
    }

    // Every payload of every rewrite-construction span in this file, with the
    // line its span started on, so the pair rule can be judged across calls.
    let mut payloads: Vec<(String, usize)> = Vec::new();
    for (call, edge) in REWRITE_CALLS {
        for open in normalized.find_call_sites(call, *edge) {
            let (from, to) = normalized.call_span(open);
            let line = normalized.line_at(open);
            for payload in normalized.payloads(from, to) {
                payloads.push((payload, line));
            }
        }
    }

    for rule in PAYLOAD_RULES {
        match rule {
            PayloadRule::Pair(left, right, pass) => {
                let source_side = payloads.iter().find(|(text, _)| text.contains(left));
                let target_side = payloads.iter().find(|(text, _)| text == right);
                if let (Some((_, line)), Some(_)) = (source_side, target_side) {
                    found.violations.push(format!(
                        "{label}:{line}: forbidden rewrite payload pair `{left}` -> `{right}` \
                         inside a rewrite call ({pass})"
                    ));
                }
            }
            PayloadRule::Single(token, pass) => {
                if let Some((_, line)) = payloads.iter().find(|(text, _)| text.contains(token)) {
                    found.violations.push(format!(
                        "{label}:{line}: forbidden rewrite payload `{token}` inside a rewrite \
                         call ({pass})"
                    ));
                }
            }
        }
    }
}
