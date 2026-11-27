//! Plain unified rows; native renderers own colors and disclosure.
use super::*;

pub(super) fn render(changes: &[Change], tool: &str, state: &DurableToolState, out: &mut Out) {
    let completed = matches!(state, DurableToolState::Completed { .. });
    let failed = matches!(state, DurableToolState::Error { .. });
    for change in changes {
        let status = if failed {
            " · attempted, not applied successfully; inspect for partial changes"
        } else if !completed {
            " · proposed"
        } else {
            ""
        };
        let added = if change.counts_known {
            change.added.to_string()
        } else {
            "?".into()
        };
        let removed = if change.counts_known {
            change.removed.to_string()
        } else {
            "?".into()
        };
        out.push(
            format!(
                "{} {} · Added {} lines, removed {} lines{status}",
                tool, change.path, added, removed,
            ),
            Mark::Header,
        );
        let numbers = change
            .rows
            .shown
            .iter()
            .filter_map(|row| match row {
                Row::Line { old, new, .. } => (*old).into_iter().chain(*new).max(),
                Row::Gap => None,
            })
            .max()
            .map_or(1, |n| n.to_string().len());
        for row in &change.rows.shown {
            match row {
                Row::Line {
                    old,
                    new,
                    mark,
                    text,
                    ended,
                } => {
                    let sign = match mark {
                        Mark::Insert => '+',
                        Mark::Delete => '-',
                        _ => ' ',
                    };
                    // Unknown offsets stay explicit; argument snippets are not
                    // a source of absolute file coordinates.
                    let coordinate = |at: Option<usize>, present: bool| {
                        if present {
                            at.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
                        } else {
                            String::new()
                        }
                    };
                    let old = coordinate(*old, *mark != Mark::Insert);
                    let new = coordinate(*new, *mark != Mark::Delete);
                    out.push(
                        format!("{old:>numbers$} {new:>numbers$} {sign}{text}"),
                        *mark,
                    );
                    if !ended {
                        let ends = if change.whole_lines {
                            "No newline at end of file"
                        } else {
                            "No line break at the end of this text"
                        };
                        out.push(format!("\\ {ends}"), Mark::Gap);
                    }
                }
                Row::Gap => out.push("⋮".into(), Mark::Gap),
            }
        }
        if change.rows.hidden > 0 {
            out.push(
                format!(
                    "… {} additional lines omitted (display limit)",
                    change.rows.hidden
                ),
                Mark::Gap,
            );
        }
    }
}
