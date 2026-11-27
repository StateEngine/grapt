use super::{cells, cut, wrap};

#[test]
fn a_wide_glyph_costs_two_cells_and_an_ascii_one_costs_one() {
    // THE MEASUREMENT THAT WAS WRONG. `日本語` is three chars and six cells.
    // Counting chars made a line believed to fit overflow its pane by one cell
    // per wide glyph, and the overflow painted over the neighbouring column.
    assert_eq!(cells("abc"), 3);
    assert_eq!(cells("日本語"), 6);
    assert_eq!(cells("a日b"), 4);
}

#[test]
fn a_combining_mark_and_a_variation_selector_cost_nothing() {
    // They modify the glyph before them. Counting them as one cell each is
    // what makes an emoji sequence measure wider than it draws.
    assert_eq!(cells("e\u{0301}"), 1, "e + combining acute is one cell");
    assert_eq!(cells("a\u{200D}b"), 2, "a zero width joiner costs nothing");
    // Except that the emoji selector ASKS for emoji presentation: the heart
    // becomes a two-cell emoji, as terminals and the screen buffer draw it.
    assert_eq!(cells("\u{2764}\u{FE0F}"), 2, "heart + emoji selector");
}

#[test]
fn text_splits_into_the_glyphs_a_terminal_draws() {
    let split = |text: &str| -> Vec<(String, usize)> {
        super::glyphs(text)
            .map(|(_, glyph, cells)| (glyph.to_owned(), cells))
            .collect()
    };
    let one = |glyph: &str, cells: usize| (glyph.to_owned(), cells);
    // One review case: the sun with its selector covers two cells, so `x` is in
    // the third, not the second.
    assert_eq!(
        split("\u{2600}\u{FE0F}x"),
        [one("\u{2600}\u{FE0F}", 2), one("x", 1)]
    );
    assert_eq!(split("e\u{0301}!"), [one("e\u{0301}", 1), one("!", 1)]);
    // A pair of regional indicators is one flag; a third starts the next.
    let flags = "\u{1F1FA}\u{1F1F8}\u{1F1EC}";
    assert_eq!(super::glyphs(flags).count(), 2);
    // Joined emoji stay one glyph rather than splitting at the joiner.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(super::glyphs(family).count(), 1);
    // Another review case: a skin tone modifies the emoji before it, joined or not.
    let thumb = "\u{1F44D}\u{1F3FD}";
    assert_eq!(split(&format!("{thumb}x")), [one(thumb, 2), one("x", 1)]);
    let coder = "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}";
    assert_eq!(split(coder), [one(coder, 2)]);
    // A control is drawn as a one-cell replacement; a halfwidth sound mark
    // takes a cell of its own, as terminals draw it.
    assert_eq!(split("a\u{7}"), [one("a", 1), one("\u{7}", 1)]);
    assert_eq!(
        split("\u{FF76}\u{FF9E}"),
        [one("\u{FF76}", 1), one("\u{FF9E}", 1)]
    );
    // A mark with nothing before it is a glyph of no cells.
    assert_eq!(split("\u{0301}a"), [one("\u{0301}", 0), one("a", 1)]);
}

#[test]
fn every_glyph_measures_as_the_screen_buffer_measures_its_symbol() {
    use unicode_width::UnicodeWidthStr;
    let sample = "a日e\u{0301}\u{2600}\u{FE0F}\u{1F1FA}\u{1F1F8}\u{1F468}\u{200D}\u{1F469}\u{FF76}x\u{2764}\u{FE0F}";
    let mut total = 0;
    for (_, glyph, cells) in super::glyphs(sample) {
        if glyph != "\u{FF9E}" && glyph != "\u{FF9F}" {
            assert_eq!(cells, glyph.width(), "{glyph:?}");
        }
        total += cells;
    }
    assert_eq!(cells(sample), total);
    // And cutting keeps whole glyphs: never the sun without its selector.
    assert_eq!(cut("\u{2600}\u{FE0F}x", 1), "");
    assert_eq!(cut("\u{2600}\u{FE0F}x", 2), "\u{2600}\u{FE0F}");
    assert_eq!(super::tail("x\u{2600}\u{FE0F}", 2), "\u{2600}\u{FE0F}");
}

#[test]
fn cutting_never_leaves_half_of_a_wide_glyph() {
    // Half a two-cell glyph is not half a character; it is a cell of garbage
    // that shifts every column after it. The glyph is DROPPED instead.
    assert_eq!(cut("日本語", 6), "日本語");
    assert_eq!(cut("日本語", 5), "日本", "the third glyph would straddle 5");
    assert_eq!(cut("日本語", 4), "日本");
    assert_eq!(
        cut("日本語", 1),
        "",
        "one cell cannot hold a two-cell glyph"
    );
    assert_eq!(cut("abc", 2), "ab");
    // And the result never EXCEEDS the budget, which is the whole point.
    for width in 0..8 {
        assert!(
            cells(&cut("a日b語c", width)) <= width,
            "cut to {width} must not exceed {width} cells"
        );
    }
}

#[test]
fn a_long_line_wraps_instead_of_being_clipped() {
    // The defect review captured: a completed edit result was a JSON row cut
    // off at the right edge, unreadable. Wrapping keeps it inspectable.
    let rows = wrap("the quick brown fox jumps", 10);
    assert!(rows.len() > 1, "it wrapped: {rows:?}");
    for row in &rows {
        assert!(cells(row) <= 10, "every row fits: {row:?}");
    }
    assert_eq!(
        rows.join(" "),
        "the quick brown fox jumps",
        "and no text was lost or duplicated: {rows:?}"
    );
}

#[test]
fn a_run_longer_than_the_pane_breaks_rather_than_hanging() {
    // A URL, a path or a JSON blob has no space to break at. Preferring a
    // space break must not mean refusing to break at all.
    let rows = wrap("aaaaaaaaaaaaaaaaaaaa", 6);
    assert_eq!(rows.len(), 4, "20 chars at 6 cells: {rows:?}");
    for row in &rows {
        assert!(cells(row) <= 6, "{row:?}");
    }
    assert_eq!(rows.concat(), "aaaaaaaaaaaaaaaaaaaa", "nothing lost");
}

#[test]
fn wrapping_wide_glyphs_counts_cells_not_characters() {
    // Five wide glyphs are TEN cells. A char-counting wrapper would put all
    // five on a six-cell row and paint four cells over the next column.
    let rows = wrap("日本語です", 6);
    for row in &rows {
        assert!(cells(row) <= 6, "row exceeds the pane: {row:?}");
    }
    assert_eq!(rows.concat(), "日本語です", "nothing lost");
    assert!(rows.len() >= 2, "ten cells cannot fit in six: {rows:?}");
}

#[test]
fn an_empty_line_stays_a_line() {
    // A blank line in a prompt is a line the user typed. Yielding no rows for
    // it silently reflows their text.
    assert_eq!(wrap("", 10), vec![String::new()]);
    assert_eq!(wrap("", 0), vec![String::new()]);
}

#[test]
fn a_zero_width_pane_is_not_an_infinite_loop() {
    // A degenerate viewport must render, not hang. `cut` to zero is empty and
    // `wrap` to zero yields one empty row rather than looping forever trying
    // to place a character that can never fit.
    assert_eq!(cut("日本", 0), "");
    assert_eq!(wrap("日本", 0), vec![String::new()]);
}

/// An independent cell ruler for the characters these tests use.
///
/// The wrap invariants must not be checked with `cells`, because a wrong width
/// table would make the function and its assertion agree and the test pass.
fn ruler(text: &str) -> usize {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            match code {
                0x0301 | 0x200D => 0,
                0x65E5 | 0x672C | 0x8A9E | 0x1F600 => 2,
                _ => 1,
            }
        })
        .sum()
}

#[test]
fn the_space_retreat_never_produces_a_row_wider_than_the_pane() {
    // ALPHA'S REPRODUCER, at his exact width. One space, fifteen ASCII, two
    // wide glyphs at width 18 produced an EMPTY row followed by a NINETEEN
    // cell row -- and the renderer's `cut` then removed the trailing glyph, so
    // a wrap meant to keep content readable silently lost it.
    //
    // The carried remainder was appended to without rechecking that the glyph
    // which forced the break still fitted.
    let input = format!(" {}\u{65E5}\u{65E5}", "a".repeat(15));
    let rows = wrap(&input, 18);
    for row in &rows {
        assert!(
            ruler(row) <= 18,
            "row exceeds the pane: {} cells in {row:?}",
            ruler(row)
        );
    }
    assert!(
        !rows[0].is_empty(),
        "no blank row is invented by retreating to a leading space: {rows:?}"
    );
    assert_eq!(
        rows.concat(),
        input,
        "and every character survives: {rows:?}"
    );
}

#[test]
fn a_glyph_too_wide_for_the_pane_is_marked_rather_than_dropped_or_overflowed() {
    // `wrap("\u{65E5}", 1)` returned a two-cell row against a one-cell bound.
    // There is no correct placement, so the choice is between losing the
    // character silently, breaking the bound, or saying so. It says so.
    let rows = wrap("\u{65E5}", 1);
    for row in &rows {
        assert!(ruler(row) <= 1, "the bound holds: {row:?}");
    }
    assert_eq!(
        rows.concat(),
        "\u{fffd}",
        "and the loss is visible: {rows:?}"
    );
}

#[test]
fn wrapping_holds_its_bound_and_its_content_across_a_sweep() {
    // THE INVARIANTS, not one case. The review finding was a specific input, and
    // fixing only that input would leave the next one to be found by eye.
    //
    // Every combination below is checked for BOTH properties at once, because
    // they are in tension: dropping a glyph keeps the bound and loses content,
    // and emitting it keeps the content and breaks the bound.
    let pieces = [
        "a",
        " ",
        "\u{65E5}",
        "ab ",
        " \u{672C}",
        "\u{1F600}",
        "  ",
        "\u{8A9E}c",
    ];
    for width in 1..=12usize {
        for a in pieces {
            for c in pieces {
                for e in pieces {
                    let input = format!("{a}{c}{e}{a}{c}");
                    let rows = wrap(&input, width);
                    for row in &rows {
                        assert!(
                            ruler(row) <= width,
                            "width {width}: row {row:?} is {} cells, input {input:?}",
                            ruler(row)
                        );
                    }
                    // Content is preserved except where a glyph could not fit
                    // a whole empty row, which is marked. Spaces at a break are
                    // consumed by the break, so they are excluded from the
                    // comparison rather than pretended to survive.
                    let expected: String = super::glyphs(&input)
                        .filter(|(_, glyph, _)| *glyph != " ")
                        .map(|(_, glyph, cost)| if cost > width { "\u{fffd}" } else { glyph })
                        .collect();
                    let joined = rows.concat();
                    let got: String = super::glyphs(&joined)
                        .filter(|(_, glyph, _)| *glyph != " ")
                        .map(|(_, glyph, _)| glyph)
                        .collect();
                    assert_eq!(
                        got, expected,
                        "width {width}: content lost or invented for {input:?}: {rows:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_zero_width_mark_before_the_break_does_not_defeat_the_cell_bound() {
    // ALPHA'S COUNTEREXAMPLE, and the reason an earlier "proof" was wrong.
    //
    // The retreat tested the space's BYTE offset. A positive byte offset does
    // not mean a cell was used: a combining mark before the space makes the
    // offset positive while the space still sits in column zero. The retreat
    // then carried seventeen cells forward, the pending glyph made nineteen
    // against a bound of eighteen, and `cut` dropped a character.
    //
    // A mutation removing the re-check had SURVIVED, and that was read as
    // proof the branch was dead. Survival means the witnesses did not
    // discriminate. This is the witness that does.
    let input = format!("\u{0301} {}\u{65E5}\u{65E5}", "a".repeat(15));
    let rows = wrap(&input, 18);
    for row in &rows {
        assert!(
            ruler(row) <= 18,
            "row is {} cells against a bound of 18: {row:?}",
            ruler(row)
        );
    }
    assert_eq!(
        rows.concat(),
        input,
        "and both wide glyphs survive: {rows:?}"
    );
}

#[test]
fn the_bound_and_the_content_hold_with_zero_width_marks_mixed_in() {
    // THE INVARIANT FAMILY, not the one literal string review sent. Zero-width
    // marks are the class that broke the cell reasoning, so they are swept
    // against the other two width classes at every small pane size.
    let pieces = [
        "a",
        " ",
        "\u{65E5}",
        "\u{0301}",
        " \u{0301}",
        "\u{0301} ",
        "a\u{0301}",
        "\u{65E5}\u{0301}",
        "\u{200D}",
    ];
    for width in 1..=10usize {
        for a in pieces {
            for c in pieces {
                for e in pieces {
                    let input = format!("{a}{c}{e}{a}{c}{e}");
                    let rows = wrap(&input, width);
                    for row in &rows {
                        assert!(
                            ruler(row) <= width,
                            "width {width}: {row:?} is {} cells, from {input:?}",
                            ruler(row)
                        );
                    }
                    let expected: String = super::glyphs(&input)
                        .filter(|(_, glyph, _)| *glyph != " ")
                        .map(|(_, glyph, cost)| if cost > width { "\u{fffd}" } else { glyph })
                        .collect();
                    let joined = rows.concat();
                    let got: String = super::glyphs(&joined)
                        .filter(|(_, glyph, _)| *glyph != " ")
                        .map(|(_, glyph, _)| glyph)
                        .collect();
                    assert_eq!(
                        got, expected,
                        "width {width}: content changed for {input:?}: {rows:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_modified_emoji_is_cut_and_wrapped_whole() {
    let thumb = "\u{1F44D}\u{1F3FD}";
    let coder = "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}";
    let text = format!("{thumb}{coder}ab");
    assert_eq!(cells(&text), 6);
    assert_eq!(cut(&text, 3), thumb);
    assert_eq!(cut(&text, 4), format!("{thumb}{coder}"));
    assert_eq!(super::tail(&text, 3), "ab");
    assert_eq!(wrap(&text, 2), [thumb, coder, "ab"]);
    // A cluster of marks past the bound continues as zero-cell glyphs.
    let zalgo = format!("a{}", "\u{301}".repeat(64));
    assert_eq!(cells(&zalgo), 1);
    assert!(super::glyphs(&zalgo).all(|(_, glyph, _)| glyph.len() <= 64));
}
