//! Every model an endpoint serves, each with the window it serves it at.
//!
//! A connect that names no model takes the endpoint's own model list. A row
//! keeps a context window only when the server states the window it actually
//! serves that model at: a loaded model's window on a local server, or the
//! window the listing reports. A trained maximum the server does not serve is
//! not a window, so such a row stays without one and reads as unknown.
use super::*;
use serde_json::{json, Value};

/// Whole-request bound for each optional read. These reads go only to the
/// user's own machine and run together, so a server that ignores one costs
/// a connect at most this long.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const BYTE_CAP: usize = 4 * 1024 * 1024;
const MAX_ROWS: usize = 1024;
/// A window above this is not a window any request budget can use.
const MAX_WINDOW: u64 = 16_777_216;

/// Why a served listing cannot back the chosen model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServedError {
    #[error("the server's model list is unreadable")]
    Unreadable,
    #[error("the server does not list the chosen model")]
    NotServed,
    #[error("the server reports no context window for the chosen model")]
    NoWindow,
}

/// How many listed models a connect that names none tries before it stops.
/// Each try is a completion, which loads the model on a local server.
const MAX_CANDIDATES: usize = 4;

/// The models a connect that names none tries, in order: `usable` listed
/// models with a stated window first, then the rest, as the listing orders
/// them.
pub fn candidate_models(listing: &[u8], usable: impl Fn(&Value) -> bool) -> Vec<String> {
    let Ok(rows) = rows(listing) else {
        return Vec::new();
    };
    let (windowed, rest): (Vec<_>, Vec<_>) = rows
        .iter()
        .filter(|row| usable(row))
        .partition(|row| listed_window(row).is_some());
    windowed
        .into_iter()
        .chain(rest)
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .take(MAX_CANDIDATES)
        .collect()
}

/// The verified listing as one row per served model, `model` first. A local
/// server is also asked for the windows its loaded models are served at, so
/// call this after the completion check has loaded `model`.
pub async fn served_listing(verified: &VerifiedKey, model: &str) -> Result<Vec<u8>, ServedError> {
    let mut rows: Vec<Value> = rows(verified.listing())?
        .iter()
        .filter_map(|row| {
            let id = row["id"].as_str()?;
            let mut served = super::endpoint_metadata::fields(row);
            served["id"] = json!(id);
            if let Some(window) = listed_window(row) {
                served["max_model_len"] = json!(window);
            }
            Some(served)
        })
        .collect();
    if rows.iter().any(|row| row.get("max_model_len").is_none()) {
        if let Some(origin) = loopback_origin(verified.base()) {
            let single = match rows.as_slice() {
                [row] => row["id"].as_str().map(str::to_owned),
                _ => None,
            };
            let windows = loaded_windows(verified, &origin, single.as_deref()).await;
            for row in &mut rows {
                let Some(id) = row["id"].as_str() else {
                    continue;
                };
                if row.get("max_model_len").is_none() {
                    if let Some(window) = windows.get(id) {
                        row["max_model_len"] = json!(window);
                    }
                }
            }
        }
    }
    let chosen = rows
        .iter()
        .position(|row| row["id"].as_str() == Some(model))
        .ok_or(ServedError::NotServed)?;
    if rows[chosen].get("max_model_len").is_none() {
        return Err(ServedError::NoWindow);
    }
    let first = rows.remove(chosen);
    rows.insert(0, first);
    let bytes =
        serde_json::to_vec(&json!({ "data": rows })).map_err(|_| ServedError::Unreadable)?;
    if bytes.len() > BYTE_CAP {
        return Err(ServedError::Unreadable);
    }
    Ok(bytes)
}

fn rows(listing: &[u8]) -> Result<Vec<Value>, ServedError> {
    let parsed: Value = serde_json::from_slice(listing).map_err(|_| ServedError::Unreadable)?;
    let rows = parsed["data"]
        .as_array()
        .filter(|rows| rows.len() <= MAX_ROWS)
        .ok_or(ServedError::Unreadable)?;
    Ok(rows.clone())
}

/// The window a listing row states it serves at: `max_model_len` is the
/// serving limit an OpenAI-compatible server such as vLLM reports.
fn listed_window(row: &Value) -> Option<u64> {
    window(row.get("max_model_len"))
}

fn window(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|window| (1..=MAX_WINDOW).contains(window))
}

/// The scheme, host and port of a literal loopback HTTP base. Server-specific
/// reads stay on the user's own machine; a remote endpoint gets none.
pub(super) fn loopback_origin(base: &str) -> Option<String> {
    let url = reqwest::Url::parse(base).ok()?;
    let host: std::net::IpAddr = url.host_str()?.trim_matches(['[', ']']).parse().ok()?;
    (url.scheme() == "http" && host.is_loopback()).then(|| url.origin().ascii_serialization())
}

/// Windows the local server reports for models it has loaded, by model id.
/// Each read is optional: a server that does not answer one reports nothing.
/// `single` names the one model a single-model server lists.
async fn loaded_windows(
    verified: &VerifiedKey,
    origin: &str,
    single: Option<&str>,
) -> std::collections::BTreeMap<String, u64> {
    let (ps, v1, v0, props_url) = (
        format!("{origin}/api/ps"),
        format!("{origin}/api/v1/models"),
        format!("{origin}/api/v0/models"),
        format!("{origin}/props"),
    );
    let (ollama, studio, studio_v0, props) = tokio::join!(
        read(verified, &ps),
        read(verified, &v1),
        read(verified, &v0),
        async {
            match single {
                Some(_) => read(verified, &props_url).await,
                None => None,
            }
        },
    );
    let mut windows = std::collections::BTreeMap::new();
    // Ollama: each running model and the window it was loaded with.
    for row in ollama
        .iter()
        .flat_map(|body| body["models"].as_array().into_iter().flatten())
    {
        if let (Some(id), Some(window)) = (row["name"].as_str(), window(row.get("context_length")))
        {
            windows.insert(id.to_owned(), window);
        }
    }
    // LM Studio: each loaded instance and the window it was loaded with,
    // under the instance id and the model key.
    for model in studio
        .iter()
        .flat_map(|body| body["models"].as_array().into_iter().flatten())
    {
        for instance in model["loaded_instances"].as_array().into_iter().flatten() {
            let Some(window) = window(instance["config"].get("context_length")) else {
                continue;
            };
            for id in [instance["id"].as_str(), model["key"].as_str()]
                .into_iter()
                .flatten()
            {
                windows.entry(id.to_owned()).or_insert(window);
            }
        }
    }
    // An older LM Studio: each model, with a window only while it is loaded.
    for row in studio_v0
        .iter()
        .flat_map(|body| body["data"].as_array().into_iter().flatten())
    {
        if let (Some(id), Some(window)) =
            (row["id"].as_str(), window(row.get("loaded_context_length")))
        {
            windows.entry(id.to_owned()).or_insert(window);
        }
    }
    // llama.cpp: its one served model and the per-request slot window.
    if let (Some(id), Some(body)) = (single, props) {
        if let Some(window) = window(body["default_generation_settings"].get("n_ctx")) {
            windows.entry(id.to_owned()).or_insert(window);
        }
    }
    windows
}

/// One bounded JSON read on the verified origin, with the verified key.
async fn read(verified: &VerifiedKey, url: &str) -> Option<Value> {
    verified.binding().check_api(url).ok()?;
    let CredentialView::ApiKey(key) = verified.secret().expose() else {
        return None;
    };
    let client = crate::chatgpt::bounded_client(false).ok()?;
    tokio::time::timeout(PROBE_TIMEOUT, async {
        let mut response = client.get(url).bearer_auth(key).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if chunk.len() > BYTE_CAP.saturating_sub(bytes.len()) {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).ok()
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests;
