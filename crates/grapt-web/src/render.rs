// Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bound HTML depth/node count before recursive conversion,
// reject non-UTF-8 text, and cap rendered output. No page scripts execute.

use crate::{FetchError, MAX_BODY_BYTES, MAX_TEXT_BYTES};

// Parsing runs away from the async executor. A cancelled conversion retains
// its slot until it finishes, so timeouts cannot accumulate background work.
static RENDER_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(crate) async fn render_async(bytes: Vec<u8>, media_type: String) -> Result<String, FetchError> {
    let permit = RENDER_SLOTS.acquire().await.map_err(|_| FetchError::Html)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        render(&bytes, &media_type)
    })
    .await
    .map_err(|_| FetchError::Html)?
}

pub(crate) fn validate_media_type(value: &str) -> Result<&str, FetchError> {
    // A semicolon inside a quoted-string (including a quoted-pair) is data,
    // not the start of another parameter. Validate each resulting field below.
    let (mut quoted, mut escaped) = (false, false);
    let mut fields = value.split(|character| {
        if escaped {
            escaped = false;
        } else {
            match character {
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                ';' if !quoted => return true,
                _ => {}
            }
        }
        false
    });
    let kind = fields.next().ok_or(FetchError::ContentType)?.trim();
    if ![
        "text/plain",
        "text/markdown",
        "text/html",
        "application/xhtml+xml",
        "application/json",
    ]
    .iter()
    .any(|allowed| kind.eq_ignore_ascii_case(allowed))
    {
        return Err(FetchError::ContentType);
    }
    for field in fields {
        let Some((name, value)) = field.split_once('=') else {
            return Err(FetchError::ContentType);
        };
        let name = name.trim_matches([' ', '\t']);
        if !media_token(name) {
            return Err(FetchError::ContentType);
        }
        let value = parameter_value(value.trim_matches([' ', '\t']))?;
        if name.eq_ignore_ascii_case("charset") && !value.eq_ignore_ascii_case("utf-8") {
            return Err(FetchError::Encoding);
        }
    }
    Ok(kind)
}

fn media_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn parameter_value(value: &str) -> Result<String, FetchError> {
    if !value.starts_with('"') {
        return media_token(value)
            .then(|| value.to_owned())
            .ok_or(FetchError::ContentType);
    }
    let body = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(FetchError::ContentType)?;
    let mut decoded = String::new();
    let mut escaped = false;
    for character in body.chars() {
        if character != '\t' && (character < ' ' || character == '\u{7f}') {
            return Err(FetchError::ContentType);
        }
        if escaped {
            decoded.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Err(FetchError::ContentType);
        } else {
            decoded.push(character);
        }
    }
    if escaped {
        return Err(FetchError::ContentType);
    }
    Ok(decoded)
}

pub(crate) fn render(bytes: &[u8], media_type: &str) -> Result<String, FetchError> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(FetchError::Oversized);
    }
    let kind = validate_media_type(media_type)?;
    let source = std::str::from_utf8(bytes).map_err(|_| FetchError::Encoding)?;
    let text = if kind.eq_ignore_ascii_case("text/html")
        || kind.eq_ignore_ascii_case("application/xhtml+xml")
    {
        let converter = htmd::HtmlToMarkdown::builder()
            .skip_tags(vec![
                "script", "style", "noscript", "svg", "iframe", "object", "embed",
            ])
            .build();
        let tree = converter
            .html_to_tree(source)
            .map_err(|_| FetchError::Html)?;
        let mut pending = vec![(tree.clone(), 0)];
        let mut count = 0;
        while let Some((node, depth)) = pending.pop() {
            count += 1;
            let children = node.children.borrow();
            if depth > 128 || count + pending.len() + children.len() > 20_000 {
                return Err(FetchError::HtmlComplexity);
            }
            pending.extend(children.iter().map(|child| (child.clone(), depth + 1)));
        }
        converter.tree_to_markdown(&tree)
    } else {
        source.to_owned()
    };
    if text.len() > MAX_TEXT_BYTES {
        return Err(FetchError::Oversized);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_links_code_and_headings_without_script_content() -> Result<(), FetchError> {
        let text = render(b"<h1>Title</h1><p><a href='https://example.com/Case'>Link</a></p><pre><code>x &lt; y</code></pre><script>bad_script()</script><style>bad_style</style><iframe>bad_frame</iframe>", "text/html; charset=UTF-8")?;
        assert!(text.contains("# Title"));
        assert!(text.contains("[Link](https://example.com/Case)"));
        assert!(text.contains("x < y"));
        assert!(!text.contains("bad_"));
        Ok(())
    }

    #[test]
    fn pathological_html_is_refused_before_recursive_rendering() {
        let deep = format!("{}text{}", "<div>".repeat(140), "</div>".repeat(140));
        assert_eq!(
            render(deep.as_bytes(), "text/html"),
            Err(FetchError::HtmlComplexity)
        );
        let wide = "<b>x</b>".repeat(11000);
        assert_eq!(
            render(wide.as_bytes(), "text/html"),
            Err(FetchError::HtmlComplexity)
        );
    }

    #[test]
    fn quoted_parameters_preserve_separators_and_validate_escapes() -> Result<(), FetchError> {
        for media_type in [
            r#"text/plain; note="a;b"; charset=utf-8"#,
            r#"TEXT/PLAIN; note="a\";b\\c"; charset="utf\-8""#,
            "text/plain; note=\"\"; charset = \"UTF-8\"",
        ] {
            assert_eq!(render("hello 界".as_bytes(), media_type)?, "hello 界");
        }
        for media_type in [
            "text/plain; note=\"unfinished",
            "text/plain; note=\"dangling\\",
            r#"text/plain; note="a"junk"#,
            "text/plain; note=raw value",
            "text/plain; =empty",
            "text/plain; note=",
            "text/plain; bad name=value",
            "text/plain; note=\"line\rbreak\"",
            "text/plain; note=\"escaped\\\nbreak\"",
            "text/plain; note=\"a\";",
        ] {
            assert_eq!(
                render(b"a", media_type),
                Err(FetchError::ContentType),
                "{media_type:?}"
            );
        }
        assert_eq!(
            render(b"a", r#"text/plain; note="a;b"; charset="latin1""#),
            Err(FetchError::Encoding)
        );
        Ok(())
    }

    #[test]
    fn text_is_preserved_and_invalid_encodings_and_binary_are_refused() -> Result<(), FetchError> {
        assert_eq!(render(b"a\n\n**b**", "text/markdown")?, "a\n\n**b**");
        assert_eq!(render(&[0xff], "text/plain"), Err(FetchError::Encoding));
        assert_eq!(
            render(b"a", "text/plain; charset=latin1"),
            Err(FetchError::Encoding)
        );
        assert_eq!(
            render(b"a", "application/octet-stream"),
            Err(FetchError::ContentType)
        );
        assert_eq!(
            render(&vec![b'a'; MAX_BODY_BYTES + 1], "text/plain"),
            Err(FetchError::Oversized)
        );
        Ok(())
    }
}
