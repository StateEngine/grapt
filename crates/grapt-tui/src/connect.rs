// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// . Modified for engine-owned
// multi-provider authentication, bounded input, and cancellable HTTP attempts.
use crate::config::Config;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_api::u3a::{
    AzureProviderRequestDto, BedrockProviderRequestDto, ConnectionInfoDto, CredentialIdDto,
    CredentialUpdateRequestDto, CustomProviderRequestDto, DatabricksProviderRequestDto,
    IntegrationAttemptStatusDto, IntegrationIdDto, IntegrationInfoDto,
    IntegrationKeyConnectRequestDto, IntegrationListResponseDto, IntegrationMethodDto,
    IntegrationOAuthAttemptDto, IntegrationOAuthCompleteRequestDto,
    IntegrationOAuthConnectRequestDto, IntegrationOAuthConnectResponseDto,
    IntegrationOAuthMethodDto, IntegrationOAuthMode, IntegrationOAuthStatusResponseDto,
    IntegrationPromptDto, IntegrationWhenDto, IntegrationWhenOperator,
};
use grapt_tui_transcript::palette::Palette;
use ratatui::{layout::Rect, Frame};
use std::{collections::BTreeMap, sync::mpsc::SyncSender};
mod cloudflare;
mod draw;
mod input;
mod worker;
use worker::{Reply, Request, Worker};

#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct Connect {
    worker: Option<Worker>,
    config: Option<Config>,
    wake: Option<SyncSender<()>>,
    pane: Option<Pane>,
    providers: Vec<IntegrationInfoDto>,
    first: bool,
    busy: bool,
    dismiss: bool,
    cancel_failed: bool,
    unconfirmed: Option<IntegrationOAuthAttemptDto>,
    pub notice: Option<String>,
}
struct Pane {
    screen: Screen,
    provider: Option<IntegrationInfoDto>,
    selected: usize,
    input: String,
    label: String,
    key_field: usize,
    endpoint: String,
    model: String,
    context: String,
    region: String,
    deployment: String,
    project: String,
    location: String,
    google_adc: bool,
    aws_chain: bool,
    azure_cli: bool,
    notice: String,
    offset: usize,
    page: usize,
}
enum Screen {
    Providers,
    Choices,
    Key,
    Gateway(cloudflare::Draft),
    Prompt {
        method: IntegrationOAuthMethodDto,
        index: usize,
        inputs: BTreeMap<String, String>,
    },
    Attempt(IntegrationOAuthAttemptDto),
    Rename(CredentialIdDto),
    Remove(CredentialIdDto),
}
enum Choice {
    GoogleAdc,
    AwsChain,
    AzureCli,
    Key(String),
    OAuth(IntegrationOAuthMethodDto),
    Rename(CredentialIdDto, String),
    Remove(CredentialIdDto, String),
}
impl Pane {
    fn new() -> Self {
        Self {
            screen: Screen::Providers,
            provider: None,
            selected: 0,
            input: String::new(),
            label: String::new(),
            key_field: 0,
            endpoint: String::new(),
            model: String::new(),
            context: String::new(),
            region: String::new(),
            deployment: String::new(),
            project: String::new(),
            location: String::new(),
            google_adc: false,
            aws_chain: false,
            azure_cli: false,
            notice: String::new(),
            offset: 0,
            page: 1,
        }
    }
    fn is_vertex(&self) -> bool {
        self.provider.as_ref().is_some_and(|p| {
            matches!(
                p.id().get().as_str(),
                "google-vertex" | "google-vertex-anthropic"
            )
        })
    }
    fn is_watsonx(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|p| p.id().get().as_str() == "watsonx")
    }
    fn is_cloud(&self) -> bool {
        self.is_vertex() || self.is_watsonx()
    }
    fn choices(&self) -> Vec<Choice> {
        let Some(provider) = &self.provider else {
            return Vec::new();
        };
        let mut rows: Vec<_> = provider
            .methods()
            .get()
            .as_slice()
            .iter()
            .filter_map(|m| match m {
                IntegrationMethodDto::Key(k) => Some(Choice::Key(
                    k.label()
                        .as_present()
                        .map(|s| s.as_str().to_owned())
                        .unwrap_or_else(|| "API key".into()),
                )),
                IntegrationMethodDto::OAuth(o) if provider.id().get().as_str() != "anthropic" => {
                    Some(Choice::OAuth(o.as_ref().clone()))
                }
                _ => None,
            })
            .collect();
        if self.azure() {
            rows.insert(0, Choice::AzureCli);
        }
        if self.is_vertex() {
            rows.insert(0, Choice::GoogleAdc);
        }
        if self.bedrock() {
            rows.insert(0, Choice::AwsChain);
        }
        for connection in provider.connections().get().as_slice() {
            if let ConnectionInfoDto::Credential(c) = connection {
                rows.push(Choice::Rename(
                    c.id().get().clone(),
                    c.label().get().as_str().into(),
                ));
                rows.push(Choice::Remove(
                    c.id().get().clone(),
                    c.label().get().as_str().into(),
                ));
            }
        }
        rows
    }
    fn reset_input(&mut self) {
        self.input.clear();
        self.google_adc = false;
        self.aws_chain = false;
        self.azure_cli = false;
        self.label.clear();
        self.key_field = 0;
        self.endpoint.clear();
        self.model.clear();
        self.context.clear();
        self.region.clear();
        self.deployment.clear();
        self.project.clear();
        self.location.clear();
        self.selected = 0;
        self.offset = 0;
        self.notice.clear();
    }
    fn custom(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|provider| provider.id().get().as_str() == "custom")
    }
    fn endpoint_provider(&self) -> Option<&'static grapt_connections::catalog::EndpointProvider> {
        self.provider.as_ref().and_then(|provider| {
            grapt_connections::catalog::endpoint_provider(provider.id().get().as_str())
        })
    }
    fn endpoint_inputs(&self) -> bool {
        self.custom() || self.endpoint_provider().is_some()
    }
    fn bedrock(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|provider| provider.id().get().as_str() == "amazon-bedrock")
    }
    fn databricks(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|provider| provider.id().get().as_str() == "databricks")
    }
    fn azure(&self) -> bool {
        self.provider.as_ref().is_some_and(|provider| {
            matches!(
                provider.id().get().as_str(),
                "azure" | "azure-cognitive-services"
            )
        })
    }
    fn key_input(&mut self) -> &mut String {
        match self.key_field {
            1 => &mut self.label,
            2 if self.is_cloud() => &mut self.project,
            3 if self.is_cloud() => &mut self.location,
            4 if self.is_cloud() => &mut self.model,
            2 => &mut self.endpoint,
            3 => &mut self.model,
            4 if self.bedrock() => &mut self.region,
            4 if self.azure() => &mut self.deployment,
            4 => &mut self.context,
            _ => &mut self.input,
        }
    }
}
impl Connect {
    pub fn start(&mut self, config: Config, wake: SyncSender<()>) -> Result<(), String> {
        self.first = true;
        self.config = Some(config.clone());
        self.wake = Some(wake.clone());
        self.worker = Some(Worker::start(config, wake)?);
        self.busy = true;
        Ok(())
    }
    pub fn visible(&self) -> bool {
        self.pane.is_some()
    }
    pub fn accepts_input(&self) -> bool {
        self.visible() && !self.busy
    }
    pub fn open(&mut self, directory: Option<&str>) {
        if self.visible() {
            return;
        }
        let directory = directory
            .map(str::to_owned)
            .or_else(|| self.config.as_ref().and_then(|c| c.directory.clone()));
        if let Some(config) = &mut self.config {
            config.directory = directory.clone();
        }
        self.dismiss = false;
        self.pane = Some(Pane::new());
        if let Some(attempt) = self.unconfirmed.take() {
            if let Some(pane) = &mut self.pane {
                pane.screen = Screen::Attempt(attempt);
                pane.notice = "Cancellation was not confirmed. Esc retries cancellation.".into();
            }
            return;
        }
        if self.busy {
            return;
        }
        if self.worker.is_none() {
            if let (Some(config), Some(wake)) = (&self.config, &self.wake) {
                match Worker::start(config.clone(), wake.clone()) {
                    Ok(worker) => {
                        self.worker = Some(worker);
                        self.busy = true;
                        return;
                    }
                    Err(error) => {
                        self.error(error);
                        return;
                    }
                }
            }
        }
        self.request(Request::List(directory));
    }
    fn request(&mut self, request: Request) {
        match self
            .worker
            .as_ref()
            .map(|w| w.requests.try_send(request).is_ok())
        {
            Some(true) => {
                self.busy = true;
                if let Some(p) = &mut self.pane {
                    p.notice = "Working… Esc cancels".into();
                }
            }
            _ => self.error("Connection worker unavailable. Close and reopen to retry.".into()),
        }
    }
    fn error(&mut self, error: String) {
        self.busy = false;
        self.cancel_failed = self.dismiss
            && self
                .pane
                .as_ref()
                .is_some_and(|p| matches!(p.screen, Screen::Attempt(_)));
        self.dismiss = false;
        if let Some(pane) = &mut self.pane {
            pane.notice = if self.cancel_failed {
                "Cancellation not confirmed. Esc closes; Ctrl+A reopens the pending attempt.".into()
            } else {
                error
            };
        } else {
            self.notice = Some(error);
        }
    }
    /// Returns (redraw, refresh model catalog). No auth data enters the composer.
    pub fn poll(&mut self) -> (bool, bool) {
        let mut changed = false;
        let mut refresh = false;
        for _ in 0..4 {
            let reply = match self.worker.as_ref().map(|w| w.replies.try_recv()) {
                Some(Ok(reply)) => reply,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.worker = None;
                    self.error(
                        "Connection service unavailable. Ctrl+A reopens connections.".into(),
                    );
                    return (true, refresh);
                }
                _ => break,
            };
            changed = true;
            self.busy = false;
            match reply {
                Reply::Catalog(mut rows, updated) => {
                    rows.sort_by(|a, b| {
                        priority(a)
                            .cmp(&priority(b))
                            .then(a.name().get().as_str().cmp(b.name().get().as_str()))
                    });
                    let fresh = self.first
                        && !rows
                            .iter()
                            .any(|r| !r.connections().get().as_slice().is_empty());
                    self.first = false;
                    self.providers = rows;
                    refresh |= updated;
                    if updated {
                        self.unconfirmed = None;
                        self.cancel_failed = false;
                    }
                    if self.dismiss {
                        self.pane = None;
                        self.dismiss = false;
                    } else if updated || fresh {
                        let mut pane = Pane::new();
                        if updated {
                            pane.notice = "Connection saved. Esc closes connections.".into();
                        }
                        self.pane = Some(pane);
                    } else if let Some(pane) = &mut self.pane {
                        pane.notice.clear();
                    }
                }
                Reply::Attempt(attempt) => {
                    if let Some(pane) = &mut self.pane {
                        pane.reset_input();
                        pane.screen = Screen::Attempt(attempt);
                    }
                    if self.dismiss {
                        self.request(Request::Cancel);
                    }
                }
                Reply::Cancelled => {
                    // Completion can win the race with Esc; re-read model truth.
                    refresh = true;
                    self.cancel_failed = false;
                    self.unconfirmed = None;
                    self.pane = None;
                    self.dismiss = false;
                }
                Reply::Notice(notice) => {
                    if self.dismiss {
                        self.request(Request::Cancel);
                    } else if let Some(p) = &mut self.pane {
                        p.notice = notice;
                    }
                }
                Reply::Failed(error) => self.error(error),
            }
        }
        (changed, refresh)
    }
    fn entries(&self) -> Vec<usize> {
        let query = self
            .pane
            .as_ref()
            .map(|p| p.input.to_lowercase())
            .unwrap_or_default();
        self.providers
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let usable = !p.connections().get().as_slice().is_empty() || p.methods().get().as_slice().iter().any(|method| {
                    matches!(method, IntegrationMethodDto::Key(_)) || matches!(method, IntegrationMethodDto::OAuth(_) if p.id().get().as_str() != "anthropic")
                });
                (usable && format!("{} {}", p.name().get().as_str(), p.id().get().as_str())
                    .to_lowercase()
                    .contains(&query))
                    .then_some(i)
            })
            .collect()
    }
    pub fn url(&self) -> Option<String> {
        match &self.pane.as_ref()?.screen {
            Screen::Attempt(attempt) => Some(attempt.url().get().as_str().into()),
            _ => None,
        }
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
        draw::draw(self, frame, area, palette);
    }
}
fn priority(provider: &IntegrationInfoDto) -> u8 {
    match provider.id().get().as_str() {
        "chatgpt" => 0,
        "openai" => 1,
        "anthropic" => 2,
        "google" => 3,
        _ => 4,
    }
}
fn condition(when: Option<&IntegrationWhenDto>, inputs: &BTreeMap<String, String>) -> bool {
    when.is_none_or(|w| {
        let equal = inputs
            .get(w.key().get().as_str())
            .is_some_and(|v| v == w.value().get().as_str());
        match *w.op().get() {
            IntegrationWhenOperator::Eq => equal,
            IntegrationWhenOperator::Neq => !equal,
        }
    })
}

#[cfg(test)]
mod tests;
