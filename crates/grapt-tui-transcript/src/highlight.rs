//! Cold grammar compilation never owns terminal input or frame rendering.
//! Source/theme keys follow the Markdown cache; only complete code fences
//! cross this bounded worker boundary, and plain text stays visible meanwhile.
use crate::palette::Palette;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};

mod grammars;

const MAX_PENDING: usize = 3;
const MAX_CACHED: usize = 16;

#[derive(Clone, PartialEq, Eq)]
struct Key {
    language: String,
    source: String,
    light: bool,
}

struct Worker {
    requests: SyncSender<Key>,
    results: Receiver<(Key, Vec<Line<'static>>)>,
}

pub struct Highlighter {
    worker: Option<Worker>,
    wake: Option<SyncSender<()>>,
    pending: Vec<Key>,
    cached: VecDeque<(Key, Vec<Line<'static>>)>,
    failed: bool,
    error: Option<String>,
    remaining: usize,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter").finish_non_exhaustive()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self {
            worker: None,
            wake: None,
            pending: Vec::new(),
            cached: VecDeque::new(),
            failed: false,
            error: None,
            remaining: MAX_CACHED,
        }
    }
}

struct NotifyOnExit(Option<SyncSender<()>>);
impl Drop for NotifyOnExit {
    fn drop(&mut self) {
        if let Some(wake) = &self.0 {
            let _ = wake.try_send(());
        }
    }
}

impl Highlighter {
    /// Completion paints cannot renew this budget, even when a source contains
    /// more distinct fences than the cache holds. Overflow stays plain until
    /// the source or viewport changes, rather than scheduling perpetual work.
    pub fn new_view(&mut self) {
        self.remaining = MAX_CACHED;
    }

    pub fn set_wake(&mut self, wake: SyncSender<()>) {
        self.wake = Some(wake);
    }

    fn start(&mut self) -> Result<(), String> {
        let (requests, input) = mpsc::sync_channel::<Key>(MAX_PENDING);
        let (output, results) = mpsc::sync_channel(MAX_PENDING);
        let wake = self.wake.clone();
        let _worker = std::thread::Builder::new()
            .name("tui-r-highlight".into())
            .spawn(move || {
                let _exit = NotifyOnExit(wake.clone());
                // Drop the result sender BEFORE notifying on exit, including
                // unwinding, so an idle UI observes Disconnected at that wake.
                let output = output;
                // Release grammar state
                // between bounded ready batches, keeping owned colored rows.
                while let Ok(first) = input.recv() {
                    let Ok(mut syntaxes) = grammars::Grammars::new() else {
                        return;
                    };
                    let mut selected = None;
                    for key in std::iter::once(first).chain(input.try_iter().take(MAX_PENDING - 1))
                    {
                        if selected
                            .as_ref()
                            .is_none_or(|(light, _)| *light != key.light)
                        {
                            let Ok(themes) = theme(key.light) else {
                                return;
                            };
                            selected = Some((key.light, themes));
                        }
                        let Some((_, themes)) = &selected else {
                            return;
                        };
                        let Ok(grammar) = syntaxes.get(&key.language) else {
                            return;
                        };
                        let rows = render(&key, grammar, themes);
                        if output.send((key, rows)).is_err() {
                            return;
                        }
                        if let Some(wake) = &wake {
                            let _ = wake.try_send(());
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        self.worker = Some(Worker { requests, results });
        Ok(())
    }

    pub fn rows(
        &mut self,
        language: &str,
        source: &str,
        palette: Palette,
    ) -> Option<Vec<Line<'static>>> {
        if self.failed || source.len() > 65_536 || language.len() > 128 {
            return None;
        }
        let key = Key {
            language: language.into(),
            source: source.into(),
            light: palette.light,
        };
        if let Some((_, rows)) = self.cached.iter().find(|(old, _)| old == &key) {
            return Some(rows.clone());
        }
        if self.pending.contains(&key) || self.pending.len() >= MAX_PENDING || self.remaining == 0 {
            return None;
        }
        if self.worker.is_none() {
            if let Err(error) = self.start() {
                self.failed = true;
                self.error = Some(format!("Syntax highlighting unavailable: {error}"));
                return None;
            }
        }
        if let Some(worker) = &self.worker {
            match worker.requests.try_send(key.clone()) {
                Ok(()) => {
                    self.remaining -= 1;
                    self.pending.push(key);
                }
                Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.failed = true;
                    self.error =
                        Some("Syntax highlighting worker stopped; showing plain text".into());
                }
            }
        }
        None
    }

    pub fn refresh(&mut self) -> Result<bool, String> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if self.failed {
            return Ok(false);
        }
        let mut changed = false;
        if let Some(worker) = &self.worker {
            for _ in 0..MAX_PENDING {
                match worker.results.try_recv() {
                    Ok((key, rows)) => {
                        self.pending.retain(|old| old != &key);
                        self.cached.push_back((key, rows));
                        while self.cached.len() > MAX_CACHED {
                            self.cached.pop_front();
                        }
                        changed = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.failed = true;
                        return Err("Syntax highlighting worker stopped; showing plain text".into());
                    }
                }
            }
        }
        Ok(changed)
    }
}

pub fn plain(source: &str, palette: Palette) -> Vec<Line<'static>> {
    source
        .split_inclusive('\n')
        .map(|line| {
            Line::styled(
                line.trim_end_matches('\n').to_owned(),
                palette.normal().bg(palette.panel),
            )
        })
        .collect()
}

fn theme(light: bool) -> Result<ThemeSet, String> {
    let bytes: &[u8] = if light {
        include_bytes!(concat!(env!("OUT_DIR"), "/theme-light.bin"))
    } else {
        include_bytes!(concat!(env!("OUT_DIR"), "/theme-dark.bin"))
    };
    syntect::dumps::from_uncompressed_data(bytes).map_err(|e| e.to_string())
}

fn render(key: &Key, syntaxes: &SyntaxSet, themes: &ThemeSet) -> Vec<Line<'static>> {
    let palette = Palette::new(key.light);
    let syntax = syntaxes
        .find_syntax_by_token(&key.language)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let name = if key.light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    let Some(theme) = themes.themes.get(name) else {
        return plain(&key.source, palette);
    };
    let mut highlight = HighlightLines::new(syntax, theme);
    key.source
        .split_inclusive('\n')
        .map(|line| {
            let spans = match highlight.highlight_line(line, syntaxes) {
                Ok(parts) => parts
                    .into_iter()
                    .map(|(style, text)| {
                        Span::styled(
                            text.trim_end_matches('\n').to_owned(),
                            Style::default()
                                .fg(Color::Rgb(
                                    style.foreground.r,
                                    style.foreground.g,
                                    style.foreground.b,
                                ))
                                .bg(palette.panel),
                        )
                    })
                    .collect(),
                Err(_) => vec![Span::styled(
                    line.trim_end_matches('\n').to_owned(),
                    palette.normal().bg(palette.panel),
                )],
            };
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests;
