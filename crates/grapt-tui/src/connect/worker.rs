//! Only the engine connect API owns authentication and credential storage.
use super::*;
use crate::config::Config;
use grapt_client::{Bounds, HttpConnection, Operation, Transport, WireRequest};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

pub enum Request {
    List(Option<String>),
    Key(IntegrationIdDto, Box<IntegrationKeyConnectRequestDto>),
    OAuth(IntegrationIdDto, IntegrationOAuthConnectRequestDto),
    Complete(IntegrationOAuthCompleteRequestDto),
    Cancel,
    Rename(CredentialIdDto, CredentialUpdateRequestDto),
    Remove(CredentialIdDto),
    Open(String),
}
pub enum Reply {
    Catalog(Vec<IntegrationInfoDto>, bool),
    Attempt(IntegrationOAuthAttemptDto),
    Cancelled,
    Notice(String),
    Failed(String),
}
pub struct Worker {
    pub requests: SyncSender<Request>,
    pub replies: Receiver<Reply>,
}
struct Api<T: Transport = HttpConnection> {
    peer: T,
    directory: Option<String>,
    attempt: Option<(IntegrationIdDto, IntegrationOAuthAttemptDto)>,
    browser: Option<std::process::Child>,
}
impl Worker {
    pub fn start(config: Config, wake: SyncSender<()>) -> Result<Self, String> {
        let (requests, incoming) = mpsc::sync_channel(2);
        let (outgoing, replies) = mpsc::sync_channel(4);
        std::thread::Builder::new()
            .name("tui-connect".into())
            .spawn(move || {
                let result = config.connect(Bounds::PRODUCTION).map(|(peer, _)| Api {
                    peer,
                    directory: config.directory.clone(),
                    attempt: None,
                    browser: None,
                });
                let mut api = match result {
                    Ok(api) => api,
                    Err(_) => {
                        let _ = outgoing.send(Reply::Failed(
                            "Connection service unavailable; reopen with Ctrl+A to retry.".into(),
                        ));
                        let _ = wake.try_send(());
                        return;
                    }
                };
                let mut next = Some(Request::List(config.directory.clone()));
                let mut polling = true;
                loop {
                    if api.browser.as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(Some(_)))) { api.browser = None; }
                    let reply = if let Some(request) = next.take() {
                        api.execute(request)
                    } else {
                        match incoming.recv_timeout(Duration::from_secs(1)) {
                            Ok(request) => {
                                polling = true;
                                if matches!(&request, Request::Cancel) || (matches!(&request, Request::List(_)) && api.attempt.is_none()) {
                                    let mut refreshed = config.clone();
                                    refreshed.expected = None;
                                    match refreshed.connect(Bounds::PRODUCTION) {
                                        Ok((peer, _)) => { api.peer = peer; api.execute(request) }
                                        Err(_) => Err("Connection service unavailable. Close and reopen to retry.".into()),
                                    }
                                } else { api.execute(request) }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout)
                                if api.attempt.is_some() && polling =>
                            {
                                api.poll()
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                let _ = api.execute(Request::Cancel);
                                return;
                            }
                        }
                    };
                    let reply = match reply {
                        Ok(None) => continue,
                        Ok(Some(reply)) => reply,
                        Err(error) => {
                            polling = false;
                            Reply::Failed(error)
                        }
                    };
                    if outgoing.send(reply).is_err() {
                        let _ = api.execute(Request::Cancel);
                        return;
                    }
                    let _ = wake.try_send(());
                }
            })
            .map_err(|_| "Could not start the connection worker")?;
        Ok(Self { requests, replies })
    }
}
impl<T: Transport> Api<T> {
    fn call(&mut self, mut request: WireRequest) -> Result<Vec<u8>, String> {
        if let Some(directory) = &self.directory {
            request = request.with_query("location[directory]", directory);
        }
        let reply = self.peer.call(&request).map_err(|_| {
            "Connection request was not confirmed. Refresh connections before retrying.".to_owned()
        })?;
        if !reply.is_success()
            && !(request.operation() == Operation::IntegrationOAuthCancel && reply.status == 404)
        {
            if reply.status == 400 && request.operation() == Operation::IntegrationKeyConnect {
                if let Ok(error) =
                    serde_json::from_slice::<grapt_api::u3a::InvalidRequestErrorDto>(&reply.body)
                {
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_KIND
                    }) {
                        return Err(grapt_api::u3a::INTEGRATION_AZURE_CREDENTIALS_MESSAGE.into());
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_GOOGLE_ADC_KIND
                    }) {
                        return Err(grapt_api::u3a::INTEGRATION_GOOGLE_ADC_MESSAGE.into());
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_KIND
                    }) {
                        return Err(grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE.into());
                    }
                    if error.kind().as_value().is_some_and(|kind| {
                        kind.as_str() == grapt_api::u3a::INTEGRATION_AWS_PROFILE_KIND
                    }) {
                        return Err(grapt_api::u3a::INTEGRATION_AWS_PROFILE_MESSAGE.into());
                    }
                }
            }
            return Err(format!(
                "Connection request failed (HTTP {}). Check the account details and try again.",
                reply.status
            ));
        }
        Ok(reply.body)
    }
    fn catalog(&mut self, changed: bool) -> Result<Reply, String> {
        let request = WireRequest::new(Operation::IntegrationList, None, None).map_err(invalid)?;
        let response: IntegrationListResponseDto = decode(&self.call(request)?)?;
        Ok(Reply::Catalog(
            response.data().get().as_slice().to_vec(),
            changed,
        ))
    }
    fn execute(&mut self, request: Request) -> Result<Option<Reply>, String> {
        let reply = match request {
            Request::List(directory) => {
                self.directory = directory;
                self.catalog(false)?
            }
            Request::Key(id, body) => {
                let request = WireRequest::integration(Operation::IntegrationKeyConnect, &id, None)
                    .map_err(invalid)?;
                self.call(request.with_body(serde_json::to_vec(&body).map_err(invalid)?))?;
                self.catalog(true)?
            }
            Request::OAuth(id, body) => {
                if self.attempt.is_some() {
                    return Err("Finish or cancel the current sign-in first.".into());
                }
                let request =
                    WireRequest::integration(Operation::IntegrationOAuthConnect, &id, None)
                        .map_err(invalid)?;
                let response: IntegrationOAuthConnectResponseDto = decode(
                    &self.call(request.with_body(serde_json::to_vec(&body).map_err(invalid)?))?,
                )?;
                self.attempt = Some((id, response.data().get().clone()));
                Reply::Attempt(response.data().get().clone())
            }
            Request::Complete(body) => {
                let (id, attempt) = self.attempt.as_ref().ok_or("No sign-in is active.")?;
                let request = WireRequest::integration(
                    Operation::IntegrationOAuthComplete,
                    id,
                    Some(attempt.attempt_id().get()),
                )
                .map_err(invalid)?;
                self.call(request.with_body(serde_json::to_vec(&body).map_err(invalid)?))?;
                return self.poll().map(|reply| {
                    reply
                        .or_else(|| Some(Reply::Notice("Waiting for sign-in… Esc cancels.".into())))
                });
            }
            Request::Cancel => {
                if let Some((id, attempt)) = &self.attempt {
                    let request = WireRequest::integration(
                        Operation::IntegrationOAuthCancel,
                        id,
                        Some(attempt.attempt_id().get()),
                    )
                    .map_err(invalid)?;
                    self.call(request)?;
                    self.attempt = None;
                }
                Reply::Cancelled
            }
            Request::Rename(id, body) => {
                let request =
                    WireRequest::credential(Operation::CredentialRename, &id).map_err(invalid)?;
                self.call(request.with_body(serde_json::to_vec(&body).map_err(invalid)?))?;
                self.catalog(true)?
            }
            Request::Remove(id) => {
                let request =
                    WireRequest::credential(Operation::CredentialRemove, &id).map_err(invalid)?;
                self.call(request)?;
                self.catalog(true)?
            }
            Request::Open(url) => {
                if self
                    .browser
                    .as_mut()
                    .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
                {
                    return Ok(Some(Reply::Notice(
                        "Browser already opened. Complete sign-in, then return here.".into(),
                    )));
                }
                self.browser = Some(open(&url)?);
                Reply::Notice("Browser opened. Complete sign-in, then return here.".into())
            }
        };
        Ok(Some(reply))
    }
    fn poll(&mut self) -> Result<Option<Reply>, String> {
        let (id, attempt) = self.attempt.as_ref().ok_or("No sign-in is active.")?;
        let request = WireRequest::integration(
            Operation::IntegrationOAuthStatus,
            id,
            Some(attempt.attempt_id().get()),
        )
        .map_err(invalid)?;
        let response: IntegrationOAuthStatusResponseDto = decode(&self.call(request)?)?;
        match response.data().get().clone() {
            IntegrationAttemptStatusDto::Pending(_) => Ok(None),
            IntegrationAttemptStatusDto::Complete(_) => {
                self.attempt = None;
                Ok(Some(self.catalog(true)?))
            }
            IntegrationAttemptStatusDto::Failed(_) | IntegrationAttemptStatusDto::Expired(_) => {
                self.attempt = None;
                Ok(Some(Reply::Failed(
                    "Sign-in failed or expired. Press Esc and try again.".into(),
                )))
            }
        }
    }
}
fn invalid(_: impl std::fmt::Display) -> String {
    "Invalid connection response or input.".into()
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(invalid)
}
fn open(value: &str) -> Result<std::process::Child, String> {
    let url = url::Url::parse(value).map_err(invalid)?;
    if value.chars().any(char::is_control)
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.scheme() == "https"
            || (url.scheme() == "http"
                && url
                    .host_str()
                    .is_some_and(|h| matches!(h, "127.0.0.1" | "localhost" | "[::1]"))))
    {
        return Err("Cannot open this sign-in URL. Copy it into your browser manually.".into());
    }
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(value)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Could not open browser. Copy the displayed URL manually.".to_owned())
}

#[cfg(test)]
mod tests;
