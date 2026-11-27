use super::*;
use grapt_client::{wire, Bounds, SessionView};

fn footers() -> Result<Vec<Slice>, Box<dyn std::error::Error>> {
    let mut view = SessionView::new(&wire::session_id("ses_000000000001")?);
    for index in 1..=10 {
        let message = wire::assistant_message(
            &format!("msg_{index:012}"),
            &[],
            Some(wire::DurableFinish::Stop),
            Some((index, 1)),
            None,
        )?;
        let page = wire::decode_messages(&wire::encode_messages(vec![message], None)?)?;
        view.apply_durable(&page.messages[0], Bounds::PRODUCTION)?;
    }
    for slice in &mut view.slices {
        let footer = slice.footer.as_mut().ok_or("footer missing")?;
        footer.execution_started_ms = Some(1_000);
        footer.completed_ms = Some(66_000);
    }
    Ok(view.slices)
}

#[test]
fn uncached_timing_footer_height_matches_painted_rows_and_scroll_reconciliation(
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = footers()?;
    for width in [8, 18, 32] {
        for light in [false, true] {
            let palette = Palette::new(light);
            let mut warm = Transcript::new();
            warm.update(true, 0, rows.clone());
            for index in 0..rows.len() {
                assert!(warm.cells[index].cache.is_none());
                let measured = warm.height(index, width);
                let painted = warm.layout(index, width, palette).len();
                assert_eq!(measured, painted, "uncached width={width} cell={index}");
            }
            for from_top in [false, true] {
                for scroll in [1, 8, 17] {
                    let mut cold = Transcript::new();
                    cold.update(true, 0, rows.clone());
                    cold.from_top = from_top;
                    cold.scroll = scroll;
                    warm.from_top = from_top;
                    warm.scroll = scroll;
                    let before = cold.visible(width, 5, palette);
                    assert_eq!(before, warm.visible(width, 5, palette));
                    // Inserting an older reply rebuilds uncached cells and
                    // restores the visible footer by its assistant identity.
                    let mut older = rows[0].clone();
                    older.footer.as_mut().ok_or("older footer missing")?.message =
                        wire::MessageWireId::parse("msg_000000000000".into())
                            .ok_or("older message identity")?;
                    let mut rebuilt = vec![older];
                    rebuilt.extend(rows.clone());
                    for (ordinal, slice) in rebuilt.iter_mut().enumerate() {
                        slice.ordinal = ordinal;
                    }
                    cold.reconcile(rebuilt);
                    assert_eq!(before, cold.visible(width, 5, palette));
                }
            }
        }
    }
    Ok(())
}
