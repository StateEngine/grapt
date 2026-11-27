// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for R's bounded finder.
mod token;
mod worker;
use crate::editor::Editor as TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::file_finder::{Limits, SearchController, SearchOutcome};
use std::{
    path::PathBuf,
    sync::mpsc::{SyncSender, TrySendError},
};
use token::Token;
use worker::{Job, Worker};

#[derive(Default)]
pub struct Mentions {
    root: Option<PathBuf>,
    active: Option<Token>,
    dismissed: Option<Token>,
    control: SearchController,
    worker: Option<Worker>,
    queued: Option<Job>,
    paths: Vec<PathBuf>,
    selected: usize,
    note: String,
}
impl Mentions {
    pub fn start(&mut self, wake: SyncSender<()>) -> Result<(), String> {
        self.worker = Some(Worker::start(wake)?);
        Ok(())
    }
    pub fn workspace(&mut self, directory: Option<&str>) -> bool {
        let root = directory.map(PathBuf::from);
        if root == self.root {
            return false;
        }
        self.close();
        self.dismissed = None;
        self.root = root;
        true
    }
    pub fn visible(&self) -> bool {
        self.active.is_some()
    }
    fn close(&mut self) {
        self.control.cancel();
        self.active = None;
        self.queued = None;
        self.paths.clear();
    }
    pub fn sync(&mut self, editor: &TextArea<'_>, enabled: bool) -> bool {
        let token = Token::at(editor);
        if self.dismissed.as_ref() != token.as_ref() {
            self.dismissed = None;
        }
        let token = token.filter(|t| enabled && self.dismissed.as_ref() != Some(t));
        if self.active == token {
            return false;
        }
        self.close();
        let Some(token) = token else {
            return true;
        };
        self.active = Some(token.clone());
        self.selected = 0;
        self.note = "Searching workspace files…".into();
        let Some(root) = &self.root else {
            self.note = "File search needs an attached workspace".into();
            return true;
        };
        match self.control.begin(root, &token.query, Limits::PRODUCTION) {
            Ok(request) if self.worker.is_some() => {
                self.queued = Some(Job {
                    token,
                    root: root.clone(),
                    request,
                });
            }
            Ok(_) => self.note = "File search worker is unavailable".into(),
            Err(error) => self.note = error.to_string(),
        }
        true
    }
    pub fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let mut changed = false;
        if let Ok(reply) = worker.replies.try_recv() {
            if reply.request.is_current()
                && self.active.as_ref() == Some(&reply.token)
                && self.root.as_ref() == Some(&reply.root)
            {
                match reply.result {
                    Ok(SearchOutcome::Ready(results)) if results.is_current() => {
                        self.note = if results.entry_limit_reached
                            || results.depth_limit_reached
                            || results.results_limited()
                        {
                            "Search limited · narrow the query · Enter/Tab insert · Esc close"
                                .into()
                        } else if results.matches.is_empty() {
                            "No matching files · Esc close".into()
                        } else {
                            "↑↓ choose · Enter/Tab insert path · Esc close".into()
                        };
                        self.paths = results
                            .matches
                            .into_iter()
                            .map(|m| m.relative_path)
                            .filter(|p| {
                                p.to_str().is_some_and(|s| !s.chars().any(char::is_control))
                            })
                            .collect();
                        changed = true;
                    }
                    Err(error) => {
                        self.note = error;
                        changed = true;
                    }
                    _ => {}
                }
            }
        }
        if let Some(job) = self.queued.take() {
            match worker.jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => self.queued = Some(job),
                Err(TrySendError::Disconnected(_)) => {
                    self.note = "File search worker stopped · Esc close".into();
                    changed = true;
                }
            }
        }
        changed
    }
    pub fn key(&mut self, key: KeyEvent, editor: &mut TextArea<'static>) -> Result<bool, String> {
        if !self.visible()
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Ok(false);
        }
        match key.code {
            KeyCode::Esc => {
                self.dismissed = self.active.clone();
                self.close();
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.paths.len().saturating_sub(1))
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => return Ok(false),
            KeyCode::Enter | KeyCode::Tab => {
                if let (Some(token), Some(path)) = (
                    &self.active,
                    self.paths.get(self.selected).and_then(|p| p.to_str()),
                ) {
                    token.insert(editor, path)?;
                    self.close();
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    pub fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        composer: ratatui::layout::Rect,
        palette: grapt_tui_transcript::palette::Palette,
    ) {
        use ratatui::{
            layout::Rect,
            text::Line,
            widgets::{Block, Borders, Clear, Paragraph},
        };
        if !self.visible() {
            return;
        }
        let height =
            (self.paths.len().min(6) as u16 + 3).min(composer.y.saturating_sub(frame.area().y));
        if height == 0 {
            return;
        }
        let area = Rect {
            y: composer.y - height,
            height,
            ..composer
        };
        let count = usize::from(height.saturating_sub(3));
        let page = self.selected.saturating_sub(count.saturating_sub(1));
        let mut lines: Vec<Line<'_>> = self
            .paths
            .iter()
            .enumerate()
            .skip(page)
            .take(count)
            .map(|(i, p)| {
                Line::styled(
                    format!(
                        "{} {}",
                        if i == self.selected { "›" } else { " " },
                        p.to_string_lossy()
                    ),
                    if i == self.selected {
                        palette.normal().fg(palette.accent)
                    } else {
                        palette.normal()
                    },
                )
            })
            .collect();
        lines.push(Line::styled(
            grapt_tui_transcript::markdown::literal(&self.note),
            palette.normal().fg(palette.muted),
        ));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .style(palette.normal().bg(palette.panel))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" @ workspace files "),
                ),
            area,
        );
    }
}

#[cfg(test)]
mod tests;
