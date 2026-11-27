use super::*;

fn compare(source: &str) {
    for light in [false, true] {
        for width in [40, 80] {
            let renderer = Markdown::new();
            let reference = Markdown::new();
            let palette = Palette::new(light);
            let mut stream = Incremental::default();
            let mut rows = vec![Line::raw("heading")];
            let mut end = 0;
            for line in source.split_inclusive('\n') {
                end += line.len();
                stream.append(&renderer, &source[..end], width, palette, &mut rows);
                assert_eq!(
                    &rows[1..],
                    reference.render(&source[..end], width, palette),
                    "width {width} source {:?}",
                    &source[..end]
                );
            }
        }
    }
}

#[test]
fn incremental_blocks_match_fresh_layout_through_markdown_reinterpretation() {
    compare("# Title\n\nFirst **bold** paragraph with 中👩🏽‍💻.\n\nSecond paragraph\ncontinued on this line.\n\n- first item\n- second item\n\nFinal paragraph\n---\n\n| A | B |\n| --- | --- |\n| one | two |\n");
}

#[test]
fn global_definitions_html_and_escaped_controls_match_the_full_source() {
    compare("[reference][later]\n\nPlain paragraph.\n\n[later]: /destination\n\nLast [reference][later].\n");
    compare("First paragraph.\n\n<div>\nhtml source\n</div>\n\nlast paragraph.\n");
    compare("First paragraph.\n\nTab\t and control \u{1b}[2J\n\nlast paragraph.\n");
}

#[test]
fn complete_blocks_advance_the_retained_boundary_but_last_block_stays_mutable() {
    let renderer = Markdown::new();
    let mut stream = Incremental::default();
    let mut rows = vec![Line::raw("heading")];
    stream.append(
        &renderer,
        "first\n\nsecond\n",
        40,
        Palette::new(false),
        &mut rows,
    );
    assert_eq!(stream.stable_source, "first\n\n".len());
    assert_eq!(stream.stable_rows, 1);
    stream.append(
        &renderer,
        "first\n\nsecond\ncontinued\n",
        40,
        Palette::new(false),
        &mut rows,
    );
    assert_eq!(stream.stable_source, "first\n\n".len());
    assert_eq!(
        &rows[1..],
        renderer.render("first\n\nsecond\ncontinued\n", 40, Palette::new(false))
    );
}

#[test]
fn indented_code_appends_and_transitions_preserve_rows_and_styles() {
    compare("first\n\n    indented\n  continuation\n");
    compare("first\n\n    indented\n    continued\n\n    more code\n\nlast paragraph\n");
    compare("first\n\n  paragraph\n  continued\n\n   ```text\n   fenced\n   ```\n\n    indented\n\n- item\n\n      nested code\n\nlast\n");
}

#[test]
fn header_only_tables_keep_their_pending_output_through_following_blocks() {
    compare("plain text\n# title\n| a | b |\n| --- | --- |\n---\nplain text\n");
    compare("first\n\n| a | b |\n| --- | --- |\n\nlast paragraph\n");
}

#[test]
fn adjacent_block_pairs_keep_the_same_source_and_output_prefix() {
    let blocks = [
        "plain text\n",
        "# title\n",
        "| a | b |\n| --- | --- |\n",
        "| a | b |\n| --- | --- |\n| c | d |\n",
        "---\n",
        "    indented\n",
        "  continuation\n",
        "- list item\n- another item\n",
        "> quoted\n",
        "```text\ncode\n```\n",
    ];
    for first in blocks {
        for second in blocks {
            compare(&format!("prefix\n\n{first}{second}\nlast paragraph\n"));
        }
    }
}

#[test]
fn open_fences_match_fresh_rows_through_closers_and_ambiguous_source() {
    for source in [
        "before\n\n```rust\nlet a = 1;\nlet 中 = \"👩🏽‍💻\";\n\n```\n\nafter\n",
        "before\n\n~~~~text\nfirst\n~~~\n  ~~~~\n\nafter\n",
        "```text\n[ref]: /location\n<div>\n```not a closer\nlast\n```\n",
        "```\nfirst\n\tindented\nlast\n```\n",
        "  ```rust\n  first\n  second\n  ```\n",
        "```rust\\name\nfirst\nsecond\n```\n",
        "[linked][ref]\n\n```rust\nfirst\nsecond\n```\n\n[ref]: /url\n",
    ] {
        compare(source);
    }
}

#[test]
fn open_fence_append_retains_prior_rows_and_discards_only_trailing_decoration() {
    let renderer = Markdown::new();
    let reference = Markdown::new();
    let palette = Palette::new(false);
    let mut stream = Incremental::default();
    let mut rows = vec![Line::raw("heading")];
    let first = "before\n\n```rust\nlet first = 1;\n";
    stream.append(&renderer, first, 40, palette, &mut rows);
    assert!(stream.open_fence.is_some());
    let row_count = rows.len();
    let content = rows
        .last()
        .and_then(|line| line.spans.first())
        .map(|span| span.content.as_ptr());
    rows.push(Line::raw("old decoration"));
    let next = format!("{first}let next = 2;\n");
    stream.append(&renderer, &next, 40, palette, &mut rows);
    assert_eq!(rows.len(), row_count + 1);
    assert_eq!(
        rows.get(row_count - 1)
            .and_then(|line| line.spans.first())
            .map(|span| span.content.as_ptr()),
        content
    );
    assert_eq!(&rows[1..], reference.render(&next, 40, palette));
    let closed = format!("{next}```\n\nafter\n");
    stream.append(&renderer, &closed, 40, palette, &mut rows);
    assert!(stream.open_fence.is_none());
    assert_eq!(&rows[1..], reference.render(&closed, 40, palette));
}

#[test]
fn flat_lists_keep_prior_items_and_match_fresh_inline_layout() {
    for width in [1, 12, 40, 80] {
        for light in [false, true] {
            let renderer = Markdown::new();
            let reference = Markdown::new();
            let palette = Palette::new(light);
            let mut stream = Incremental::default();
            let mut rows = vec![Line::raw("heading")];
            let mut source = String::new();
            for index in 0..32 {
                let previous = rows.clone();
                source.push_str(&format!(
                    "- Shared row {index}: **bold** and `code`, 中文 👩🏽‍💻 é.\n"
                ));
                let retained = stream.append(&renderer, &source, width, palette, &mut rows);
                if index > 0 {
                    assert_eq!(retained, previous.len(), "prior list items stay painted");
                    assert_eq!(&rows[..retained], previous);
                }
                assert_eq!(&rows[1..], reference.render(&source, width, palette));
            }
        }
    }
}

#[test]
fn flat_list_append_yields_to_container_changes_and_source_wide_syntax() {
    for prefix in ["", "# Heading\n\n", "[ref]: /earlier\n\n"] {
        for suffix in [
            "\n- Loose item\n",
            "  continued text\n- Another item\n",
            "  - Nested item\n- Another item\n",
            "* Changed marker\n",
            "- ---\n",
            "- [x] Task item\n",
            "- Heading\n  ---\n",
            "- ```rust\n  fn main() {}\n  ```\n",
            "- Html <b>item</b>\n",
            "- Escape \u{1b}[2J control\n",
            "\n[ref]: /replacement\n",
            "\nAfter list\n",
        ] {
            compare(&format!(
                "{prefix}- First **item**\n- Second [link][ref]\n{suffix}"
            ));
        }
    }
    compare("- Apple `unclosed\n- Banana `closing\n- Cherry **unclosed\n- Date **closing\n");
}

#[test]
fn completed_flat_list_keeps_its_rows_when_a_following_paragraph_starts() {
    for width in [1, 12, 40, 80] {
        for light in [false, true] {
            let renderer = Markdown::new();
            let palette = Palette::new(light);
            let mut stream = Incremental::default();
            let mut rows = vec![Line::raw("heading")];
            let mut source = String::from("- First **item**\n- Second item 中文\n");
            stream.append(&renderer, &source, width, palette, &mut rows);
            let previous = rows.clone();
            source.push('\n');
            stream.append(&renderer, &source, width, palette, &mut rows);
            assert_eq!(
                stream.stable_source, 0,
                "a blank alone does not close the list"
            );
            source.push_str("After list.\n");
            let retained = stream.append(&renderer, &source, width, palette, &mut rows);
            assert_eq!(retained, previous.len());
            assert_eq!(&rows[..retained], previous);
            assert_eq!(&rows[1..], renderer.render(&source, width, palette));
        }
    }
}

#[test]
fn pending_list_blanks_preserve_loose_items_and_later_global_reinterpretation() {
    for suffix in [
        "\n\n- Loose item\n",
        "\n  Indented continuation\n",
        "\nAfter [link][ref].\n\n[ref]: /destination\n",
        "\nAfter inline <b>html</b>.\n",
        "\nAfter paragraph\n---\n",
        "\nAfter paragraph\n| --- |\n",
        "\n[ref]: /destination\n",
        "\n<div>\nhtml\n</div>\n",
    ] {
        compare(&format!("- First [link][ref]\n- Second item\n{suffix}"));
    }
}

#[test]
fn plain_paragraphs_retain_wrapped_rows_while_matching_fresh_unicode_layout() {
    for width in [1, 12, 40, 80] {
        for light in [false, true] {
            let renderer = Markdown::new();
            let reference = Markdown::new();
            let palette = Palette::new(light);
            let mut stream = Incremental::default();
            let mut rows = vec![Line::raw("heading")];
            let first = "Streaming 中文 👩🏽‍💻 é text. ".repeat(8).trim_end().to_owned() + "\n";
            stream.append(&renderer, &first, width, palette, &mut rows);
            let original = rows[1].spans[0].content.as_ptr();
            let mut source = first;
            for _ in 0..12 {
                source.push_str("More 中文 👩🏽‍💻 é content.\n");
                let retained = stream.append(&renderer, &source, width, palette, &mut rows);
                assert!(retained > 1, "completed wrapped rows must survive append");
                assert_eq!(rows[1].spans[0].content.as_ptr(), original);
                assert_eq!(&rows[1..], reference.render(&source, width, palette));
            }
        }
    }
}

#[test]
fn paragraph_fast_path_yields_to_reinterpreting_syntax_and_global_definitions() {
    for prefix in [
        "",
        "# heading\n\n",
        "[label][ref]\n\n[ref]: /first\n\n",
        "| a | b |\n| --- | --- |\n\n",
    ] {
        for suffix in [
            "More text.\n\nNew paragraph.\n",
            "---\n\nSetext changes the preceding paragraph.\n",
            "| separator |\n| --- |\n",
            "More **bold** and `code`.\n",
            "[ref]: /replacement\n",
            "  indented continuation\n",
            "Hard break  \nnext line\n",
            "Escaped &amp; and \\*literal.\n",
            "Control \u{1b}[2J and tab\t.\n",
            "Bidirectional \u{202e} control.\n",
        ] {
            compare(&format!(
                "{prefix}Simple paragraph line.\nMore plain text.\n{suffix}"
            ));
        }
    }
}

#[test]
fn cold_lists_match_full_layout_and_later_reinterpretation() {
    let items = (0..80)
        .map(|n| format!("- Item {n}: **bold** and `code` 中👩🏽‍💻.\n"))
        .collect::<String>();
    for prefix in [
        "",
        "# Heading\n\nParagraph.\n\n",
        "```python\ndef render() -> str:\n    return \"text\"\n```\n\n",
        "```text\n\n",
    ] {
        for suffix in ["", "\n", "\nFinal paragraph_with_emphasis.\n"] {
            let source = format!("{prefix}{items}{suffix}");
            assert!(super::super::list::cold_run(&source).is_some());
            for light in [false, true] {
                for width in [0, 1, 12, 80] {
                    let renderer = Markdown::new();
                    let palette = Palette::new(light);
                    let mut stream = Incremental::default();
                    let mut rows = vec![Line::raw("heading")];
                    stream.append(&renderer, &source, width, palette, &mut rows);
                    assert_eq!(
                        &rows[1..],
                        renderer.render(&source, width, palette),
                        "{prefix:?} {suffix:?} width {width}"
                    );
                    assert_eq!(renderer.height(&source, width), rows.len() - 1);
                    for extra in [
                        "\n- Later item\n",
                        "\n    nested continuation\n",
                        "\n[later]: /url\n",
                        "\n<div>html</div>\n",
                    ] {
                        let appended = format!("{source}{extra}");
                        stream.append(&renderer, &appended, width, palette, &mut rows);
                        assert_eq!(&rows[1..], renderer.render(&appended, width, palette));
                        // Each transition begins with a fresh cold layout.
                        stream = Incremental::default();
                        stream.append(&renderer, &source, width, palette, &mut rows);
                    }
                }
            }
        }
    }
}

#[test]
fn ambiguous_cold_list_shapes_keep_the_full_parser() {
    let items = "- Item **bold**.\n".repeat(40);
    for source in [
        format!("[ref]: /url\n\n{items}"),
        format!("<div>\n\n{items}"),
        format!("tab\t\n\n{items}"),
        format!("\u{202e}\n\n{items}"),
        format!("{items}\n- loose item\n"),
        format!("{items}  nested continuation\n"),
        format!("{items}\n# Heading\n"),
        format!("{items}\n{}", "paragraph ".repeat(500)),
        format!("{}\n\n{items}", "prefix ".repeat(700)),
        "- Short list\n".repeat(8),
    ] {
        assert!(super::super::list::cold_run(&source).is_none());
        let renderer = Markdown::new();
        let mut rows = vec![Line::default()];
        Incremental::default().append(&renderer, &source, 40, Palette::new(false), &mut rows);
        assert_eq!(
            &rows[1..],
            renderer.render(&source, 40, Palette::new(false))
        );
    }
}
