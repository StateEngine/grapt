use super::*;

#[test]
fn paragraphs_have_one_gap_without_leading_trailing_or_soft_break_gaps() {
    for light in [false, true] {
        let renderer = Markdown::new();
        let p = Palette::new(light);
        for source in ["First.\n\nSecond.", "\nFirst.\n\n\n\nSecond.\n\n"] {
            let rows = renderer.render(source, 40, p);
            assert_eq!(
                rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
                ["First.", "", "Second."]
            );
            assert_eq!(renderer.height(source, 40), 3);
        }
        assert_eq!(renderer.render("Soft\nbreak.", 40, p).len(), 1);
        assert_eq!(renderer.render("Hard  \nbreak.", 40, p).len(), 2);
        assert_eq!(renderer.render("- First\n- Second\n", 40, p).len(), 2);
        for source in ["> First.\n>\n> Second.", "- First.\n\n- Second."] {
            let rows = renderer.render(source, 40, p);
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[1].width(), 0);
        }
    }
}

#[test]
fn incremental_separators_survive_appends_and_block_reinterpretation() {
    for source in [
        "First.\n\nSecond.\nMore text.\n\nThird.\n",
        "First.\n\nTitle\n---\n\nAfter title.\n",
        "- First\n- Second\n\nAfter list.\n\nLast.\n",
        "Before.\n\n```text\ncode\n```\n\nAfter.\n",
    ] {
        for width in [1, 8, 40] {
            let renderer = Markdown::new();
            let p = Palette::new(false);
            let mut state = Incremental::default();
            let mut rows = vec![Line::default()];
            let mut end = 0;
            for line in source.split_inclusive('\n') {
                end += line.len();
                state.append(&renderer, &source[..end], width, p, &mut rows);
                assert_eq!(
                    &rows[1..],
                    renderer.render(&source[..end], width, p),
                    "{width} {:?}",
                    &source[..end]
                );
                assert_eq!(renderer.height(&source[..end], width), rows.len() - 1);
            }
        }
    }
}
