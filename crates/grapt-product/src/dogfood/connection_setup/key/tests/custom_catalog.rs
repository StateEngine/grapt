//! A custom connect carries the endpoint's own catalogue facts into the
//! installed discovery listing, and unknowns stay unknown.
use crate::dogfood::connection_setup::{key::KeyRequest, ConnectionSetup};
use grapt_api::u3a::{
    IntegrationIdDto, IntegrationKeyConnectRequestDto, ProjectCurrentLocationQueryDto,
};
use grapt_connections::store::Store;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MODEL: &str = "vendor/model";

/// Answer the discovery read and the completion check on one loopback origin.
async fn serve(
    listener: tokio::net::TcpListener,
    models_body: &'static str,
    completion_status: &'static str,
    seen: tokio::sync::mpsc::Sender<&'static str>,
) {
    for _ in 0..2 {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") && request.len() <= 16_384 {
            let Ok(byte) = socket.read_u8().await else {
                return;
            };
            request.push(byte);
        }
        let head = String::from_utf8_lossy(&request);
        let is_get = head.starts_with("GET ");
        if seen
            .send(if is_get { "GET" } else { "POST" })
            .await
            .is_err()
        {
            return;
        }
        let status = if is_get { "200 OK" } else { completion_status };
        let body = if is_get {
            models_body
        } else {
            // The completion check posts a small chat request.
            "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":\"stop\"}]}"
        };
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if socket.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn connect_and_read_listing(
    models_body: &'static str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "grapt-custom-catalog-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    grapt_login::profile_security::isolate_test_keystore()?;
    crate::dogfood::first_use::initialize_empty_profile(&root)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let (seen, _requests) = tokio::sync::mpsc::channel(2);
    let server = tokio::spawn(serve(listener, models_body, "200 OK", seen));
    let outcome = connect(&root, base).await?;
    server.abort();
    let records = Store::read_snapshot(&root)?.ok_or("installed connection metadata is missing")?;
    let connection = records
        .connections
        .iter()
        .find(|record| record.catalog_provider.as_str() == "custom")
        .ok_or("custom connection was not installed")?;
    let listing = std::fs::read(root.join(&connection.discovery_file))?;
    let parsed = serde_json::from_slice(&listing)?;
    let _ = std::fs::remove_dir_all(&root);
    outcome.map_err(|error| format!("connect refused: {error:?}"))?;
    Ok(parsed)
}

#[tokio::test]
async fn reported_endpoint_metadata_lands_in_the_installed_listing(
) -> Result<(), Box<dyn std::error::Error>> {
    let listing = connect_and_read_listing(
        "{\"data\":[{\"id\":\"vendor/model\",\"context_length\":131072,\
         \"supported_parameters\":[\"tools\",\"temperature\"],\
         \"top_provider\":{\"max_completion_tokens\":16384},\
         \"architecture\":{\"input_modalities\":[\"text\"],\"output_modalities\":[\"text\"]}}]}",
    )
    .await?;
    let row = &listing["data"][0];
    assert_eq!(row["id"], MODEL);
    // The explicit window keeps its place; the reported facts join it.
    assert_eq!(row["max_model_len"], 500000);
    assert_eq!(row["context_length"], 131072);
    assert_eq!(row["max_completion_tokens"], 16384);
    assert_eq!(row["supported_parameters"], json!(["tools", "temperature"]));
    assert_eq!(row["input_modalities"], json!(["text"]));
    Ok(())
}

/// A hostile /models reply may report anything, but the user's explicit window
/// is never widened by it.
#[tokio::test]
async fn a_reported_window_never_widens_the_explicit_one() -> Result<(), Box<dyn std::error::Error>>
{
    let reply = "{\"data\":[{\"id\":\"vendor/model\",\"context_length\":999999999,\
                 \"max_model_len\":999999999}]}";
    let listing = connect_and_read_listing(reply).await?;
    let row = &listing["data"][0];
    assert_eq!(row["max_model_len"], 500000, "the explicit window moved");
    Ok(())
}

#[tokio::test]
async fn a_failed_discovery_never_fails_the_connect_and_invents_nothing(
) -> Result<(), Box<dyn std::error::Error>> {
    let listing = connect_and_read_listing("HTTP-body-that-is-not-json").await?;
    let row = &listing["data"][0];
    assert_eq!(row["id"], MODEL);
    assert_eq!(row["max_model_len"], 500000);
    for absent in [
        "context_length",
        "max_completion_tokens",
        "supported_parameters",
    ] {
        assert!(row.get(absent).is_none(), "{absent} was invented");
    }
    Ok(())
}

async fn connect(
    root: &std::path::Path,
    base: String,
) -> Result<
    Result<grapt_api::u3a::NoContent, grapt_api::u3a::InvalidRequestErrorDto>,
    Box<dyn std::error::Error>,
> {
    let limits = crate::dogfood::production::ProductionProviderLimits {
        connect: std::time::Duration::from_secs(5),
        response_head: std::time::Duration::from_secs(5),
        active_attempts: std::num::NonZeroUsize::new(1).ok_or("zero limit")?,
        child_attempts: std::num::NonZeroUsize::new(1).ok_or("zero limit")?,
    };
    let (owner, _driver) =
        ConnectionSetup::administration(root, crate::dogfood::event_hub()?, limits, false);
    let request: IntegrationKeyConnectRequestDto = serde_json::from_value(json!({
        "key":"fixture-key",
        "custom":{"endpoint":base,"model":MODEL,"context_window":500000}
    }))?;
    let query: ProjectCurrentLocationQueryDto =
        serde_json::from_value(json!({"location":{"directory":root.to_string_lossy()}}))?;
    let id: IntegrationIdDto = serde_json::from_value(json!("custom"))?;
    Ok(owner
        .connect_key_owned(id, query, KeyRequest::Entered(Box::new(request)), None)
        .await)
}

#[tokio::test]
async fn review_a_rejected_key_does_not_trigger_catalog_discovery(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "grapt-custom-catalog-rejected-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    crate::dogfood::first_use::initialize_empty_profile(&root)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let (seen, mut requests) = tokio::sync::mpsc::channel(2);
    let server = tokio::spawn(serve(listener, "{}", "401 Unauthorized", seen));
    let outcome = connect(&root, base).await?;
    server.abort();
    let _ = std::fs::remove_dir_all(&root);

    assert!(outcome.is_err());
    assert_eq!(
        requests.try_recv()?,
        "POST",
        "verification must happen first"
    );
    assert!(
        requests.try_recv().is_err(),
        "a refused key needs no catalog request"
    );
    Ok(())
}
