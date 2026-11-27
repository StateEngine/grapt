//! One subdued footer for a completed assistant reply. The optional completion
//! span is the seam for the execution owner's Worked/done text.
use crate::{markdown, palette::Palette};
use grapt_client::wire::AssistantFooter;
use ratatui::text::{Line, Span};

mod completion;

pub(crate) fn reply(footer: &AssistantFooter, width: u16, palette: Palette) -> Vec<Line<'static>> {
    lines(footer, width, palette, completion(footer, palette))
}

pub(crate) fn completion(footer: &AssistantFooter, palette: Palette) -> Option<Span<'static>> {
    completion::summary(
        footer.execution_started_ms,
        footer.completed_ms,
        footer.outcome.as_ref(),
    )
    .map(|text| Span::styled(text, palette.normal().fg(palette.muted)))
}

pub fn lines(
    footer: &AssistantFooter,
    width: u16,
    palette: Palette,
    completion: Option<Span<'static>>,
) -> Vec<Line<'static>> {
    let style = palette.normal().fg(palette.muted);
    let mut spans = Vec::new();
    if let Some(completion) = completion {
        spans.push(completion);
        spans.push(Span::styled(" · ", style));
    }
    spans.push(Span::styled(
        markdown::literal(&grapt_client::view::assistant_footer::text(footer)),
        style,
    ));
    // The old AssistantFooter uses paddingLeft=3, middle-dot separators and
    // text.subdued. Preserve those tokens in both default themes.
    let indent = width.saturating_sub(1).min(3);
    let mut rows = markdown::wrap(vec![Line::from(spans)], width.saturating_sub(indent));
    for line in &mut rows {
        line.spans
            .insert(0, Span::styled(" ".repeat(indent as usize), style));
    }
    rows.push(Line::default());
    rows
}

#[cfg(test)]
mod tests;
