//! A local server preset connected without a model offers every model it
//! serves, and a reconnect never keeps a default that can no longer run.
use crate::dogfood::connection_setup::{key::KeyRequest, ConnectionSetup};
use grapt_api::u3a::{
    IntegrationIdDto, IntegrationKeyConnectRequestDto, ProjectCurrentLocationQueryDto,
};
use grapt_connections::store::Store;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// An Ollama-shaped loopback server: it lists two models, loads the model a
/// completion names, and reports a window only for loaded models.
async fn ollama() -> Result<(String, Arc<Mutex<Vec<String>>>), std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let loaded = Arc::new(Mutex::new(Vec::<String>::new()));
    let state = Arc::clone(&loaded);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") && head.len() <= 16_384 {
                let Ok(byte) = socket.read_u8().await else {
                    return;
                };
                head.push(byte);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            let mut body = vec![0; length.min(1 << 20)];
            if socket.read_exact(&mut body).await.is_err() {
                return;
            }
            let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
            let reply = match path.as_str() {
                "/v1/models" => json!({"data":[{"id":"qwen:latest"},{"id":"llama:latest"}]}),
                "/v1/chat/completions" => {
                    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
                    if let (Some(model), Ok(mut loaded)) = (request["model"].as_str(), state.lock())
                    {
                        loaded.push(model.to_owned());
                    }
                    json!({"choices":[{"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}]})
                }
                "/api/ps" => {
                    let loaded = state
                        .lock()
                        .map(|loaded| loaded.clone())
                        .unwrap_or_default();
                    json!({"models": loaded.iter().map(|name| json!({"name":name,"context_length":65536})).collect::<Vec<_>>()})
                }
                _ => Value::Null,
            };
            let reply = if reply.is_null() {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            } else {
                let body = reply.to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            if socket.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    Ok((base, loaded))
}

async fn connect(root: &std::path::Path, custom: Value) -> TestResult {
    let limits = crate::dogfood::production::ProductionProviderLimits {
        connect: std::time::Duration::from_secs(5),
        response_head: std::time::Duration::from_secs(5),
        active_attempts: std::num::NonZeroUsize::new(1).ok_or("zero limit")?,
        child_attempts: std::num::NonZeroUsize::new(1).ok_or("zero limit")?,
    };
    let (owner, _driver) =
        ConnectionSetup::administration(root, crate::dogfood::event_hub()?, limits, false);
    let request: IntegrationKeyConnectRequestDto =
        serde_json::from_value(json!({"key":"", "custom":custom}))?;
    let query: ProjectCurrentLocationQueryDto =
        serde_json::from_value(json!({"location":{"directory":root.to_string_lossy()}}))?;
    let id: IntegrationIdDto = serde_json::from_value(json!("ollama"))?;
    owner
        .connect_key_owned(id, query, KeyRequest::Entered(Box::new(request)), None)
        .await
        .map(|_| ())
        .map_err(|error| {
            let body = serde_json::to_string(&error).unwrap_or_default();
            format!("connect refused: {body}").into()
        })
}

fn installed(root: &std::path::Path) -> Result<(Value, String), Box<dyn std::error::Error>> {
    let records = Store::read_snapshot(root)?.ok_or("installed connection metadata is missing")?;
    let connection = records
        .connections
        .iter()
        .find(|record| record.catalog_provider.as_str() == "ollama")
        .ok_or("the preset connection was not installed")?;
    let listing = serde_json::from_slice(&std::fs::read(root.join(&connection.discovery_file))?)?;
    let default = records
        .default_model
        .ok_or("no default model was chosen")?
        .model
        .as_str()
        .to_owned();
    Ok((listing, default))
}

#[tokio::test]
async fn a_preset_without_a_model_offers_every_served_model_and_moves_an_unrunnable_default(
) -> TestResult {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("profile");
    crate::dogfood::first_use::initialize_empty_profile(&root)?;
    let (base, loaded) = ollama().await?;

    connect(&root, json!({"endpoint": base})).await?;
    let (listing, default) = installed(&root)?;
    assert_eq!(default, "qwen:latest");
    assert_eq!(listing["data"][0]["id"], "qwen:latest");
    assert_eq!(listing["data"][0]["max_model_len"], 65536);
    assert_eq!(listing["data"][1]["id"], "llama:latest");
    assert!(listing["data"][1].get("max_model_len").is_none());

    // The server unloads the first model; connecting the second by name
    // must not leave the default on a model that now has no window.
    loaded.lock().map_err(|_| "fixture state poisoned")?.clear();
    connect(&root, json!({"endpoint": base, "model": "llama:latest"})).await?;
    let (listing, default) = installed(&root)?;
    assert_eq!(listing["data"][0]["id"], "llama:latest");
    assert_eq!(listing["data"][0]["max_model_len"], 65536);
    assert_eq!(default, "llama:latest");
    Ok(())
}

#[tokio::test]
async fn a_window_without_a_model_is_refused_before_any_request() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("profile");
    crate::dogfood::first_use::initialize_empty_profile(&root)?;
    let outcome = connect(
        &root,
        json!({"endpoint": "http://127.0.0.1:9/v1", "context_window": 65536}),
    )
    .await;
    assert!(outcome.is_err_and(|error| error.to_string().contains("belongs to a named model")));
    Ok(())
}
