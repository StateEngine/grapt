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

/// What the Ollama-shaped server lists and how it answers.
#[derive(Debug)]
struct Server {
    /// The `/v1/models` rows.
    listing: Value,
    /// Models whose completion is refused, as an embedding model's is.
    refused: Vec<String>,
    /// Models a completion loaded.
    loaded: Vec<String>,
    /// Whether `/api/ps` reports loaded models' windows.
    windows: bool,
}

type Shared = Arc<Mutex<Server>>;

/// An Ollama-shaped loopback server: it lists its models, loads the model a
/// completion names, and reports a window only for loaded models.
async fn ollama(listing: Value) -> Result<(String, Shared), std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let shared = Arc::new(Mutex::new(Server {
        listing,
        refused: Vec::new(),
        loaded: Vec::new(),
        windows: true,
    }));
    let state = Arc::clone(&shared);
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
            let reply = respond(&state, &path, &body);
            let reply = match reply {
                Some(reply) => {
                    let body = reply.to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                None => {
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                }
            };
            if socket.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    Ok((base, shared))
}

/// The server's answer to one request; the lock never spans an await.
fn respond(state: &Shared, path: &str, body: &[u8]) -> Option<Value> {
    let mut server = state.lock().ok()?;
    match path {
        "/v1/models" => Some(server.listing.clone()),
        "/v1/chat/completions" => {
            let request: Value = serde_json::from_slice(body).unwrap_or_default();
            let model = request["model"].as_str().unwrap_or_default().to_owned();
            if server.refused.contains(&model) {
                return None;
            }
            server.loaded.push(model);
            Some(
                json!({"choices":[{"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}]}),
            )
        }
        "/api/ps" if server.windows => Some(json!({"models": server
            .loaded
            .iter()
            .map(|name| json!({"name":name,"context_length":65536}))
            .collect::<Vec<_>>()})),
        _ => None,
    }
}

fn two_models() -> Value {
    json!({"data":[{"id":"qwen:latest"},{"id":"llama:latest"}]})
}

fn profile() -> Result<(tempfile::TempDir, std::path::PathBuf), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("profile");
    crate::dogfood::first_use::initialize_empty_profile(&root)?;
    Ok((temporary, root))
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
    let (_temporary, root) = profile()?;
    let (base, server) = ollama(two_models()).await?;

    connect(&root, json!({"endpoint": base})).await?;
    let (listing, default) = installed(&root)?;
    assert_eq!(default, "qwen:latest");
    assert_eq!(listing["data"][0]["id"], "qwen:latest");
    assert_eq!(listing["data"][0]["max_model_len"], 65536);
    assert_eq!(listing["data"][1]["id"], "llama:latest");
    assert!(listing["data"][1].get("max_model_len").is_none());

    // The server unloads the first model; connecting the second by name
    // must not leave the default on a model that now has no window.
    server
        .lock()
        .map_err(|_| "fixture state poisoned")?
        .loaded
        .clear();
    connect(&root, json!({"endpoint": base, "model": "llama:latest"})).await?;
    let (listing, default) = installed(&root)?;
    assert_eq!(listing["data"][0]["id"], "llama:latest");
    assert_eq!(listing["data"][0]["max_model_len"], 65536);
    assert_eq!(default, "llama:latest");
    Ok(())
}

#[tokio::test]
async fn a_listed_model_that_cannot_complete_gives_way_to_the_next() -> TestResult {
    let (_temporary, root) = profile()?;
    let (base, server) =
        ollama(json!({"data":[{"id":"embed:latest"},{"id":"qwen:latest"}]})).await?;
    server
        .lock()
        .map_err(|_| "fixture state poisoned")?
        .refused
        .push("embed:latest".into());
    connect(&root, json!({"endpoint": base})).await?;
    let (listing, default) = installed(&root)?;
    assert_eq!(default, "qwen:latest");
    assert_eq!(listing["data"][0]["id"], "qwen:latest");
    Ok(())
}

#[tokio::test]
async fn a_reconnect_moves_a_default_whose_tools_the_live_listing_dropped() -> TestResult {
    let (_temporary, root) = profile()?;
    let tools = json!(["tools", "temperature"]);
    let (base, server) = ollama(json!({"data":[
        {"id":"qwen:latest","supported_parameters":tools},
        {"id":"llama:latest","supported_parameters":tools},
    ]}))
    .await?;
    connect(&root, json!({"endpoint": base})).await?;
    connect(&root, json!({"endpoint": base, "model": "llama:latest"})).await?;
    assert_eq!(
        installed(&root)?.1,
        "qwen:latest",
        "a runnable default stays"
    );
    // The live listing now says the default takes no tools.
    {
        let mut server = server.lock().map_err(|_| "fixture state poisoned")?;
        server.listing = json!({"data":[
            {"id":"qwen:latest","supported_parameters":["temperature"]},
            {"id":"llama:latest","supported_parameters":tools},
        ]});
    }
    connect(&root, json!({"endpoint": base, "model": "llama:latest"})).await?;
    assert_eq!(installed(&root)?.1, "llama:latest");
    Ok(())
}

#[tokio::test]
async fn a_refusal_never_repeats_a_model_identity_the_endpoint_returned() -> TestResult {
    let (_temporary, root) = profile()?;
    let (base, server) = ollama(json!({"data":[{"id":"endpoint-chosen-name"}]})).await?;
    server.lock().map_err(|_| "fixture state poisoned")?.windows = false;
    let refusal = match connect(&root, json!({"endpoint": base})).await {
        Err(refusal) => refusal.to_string(),
        Ok(()) => return Err("a model with no window was connected".into()),
    };
    assert!(refusal.contains("no context window"), "{refusal}");
    assert!(!refusal.contains("endpoint-chosen-name"), "{refusal}");
    Ok(())
}

#[tokio::test]
async fn a_window_without_a_model_is_refused_before_any_request() -> TestResult {
    let (_temporary, root) = profile()?;
    let outcome = connect(
        &root,
        json!({"endpoint": "http://127.0.0.1:9/v1", "context_window": 65536}),
    )
    .await;
    assert!(outcome.is_err_and(|error| error.to_string().contains("belongs to a named model")));
    Ok(())
}
