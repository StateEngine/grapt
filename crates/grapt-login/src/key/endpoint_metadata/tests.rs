//! Optional discovery keeps every failure an unchanged listing.
use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve exactly one request head, then reply and close.
async fn serve_one(listener: tokio::net::TcpListener, reply: String) -> String {
    let (mut socket, _) = listener
        .accept()
        .await
        .unwrap_or_else(|error| panic!("fixture accept failed: {error}"));
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") && request.len() <= 8192 {
        let byte = socket
            .read_u8()
            .await
            .unwrap_or_else(|error| panic!("fixture read failed: {error}"));
        request.push(byte);
    }
    socket
        .write_all(reply.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
    String::from_utf8(request).unwrap_or_else(|error| panic!("request is not text: {error}"))
}

fn listing() -> Vec<u8> {
    serde_json::to_vec(&json!({"data":[{"id":"vendor/model","max_model_len":500000}]}))
        .unwrap_or_else(|error| panic!("fixture listing encoding failed: {error}"))
}

fn secret() -> VaultSecret {
    VaultSecret::api_key("fixture-key".to_owned().into())
        .unwrap_or_else(|error| panic!("fixture key refused: {error}"))
}

/// A verified key as the connect flow holds it once the check has passed. The
/// discovery read cannot be reached any other way, which is the property under
/// test at the product seam.
fn verified(listing: Vec<u8>, base: &str) -> VerifiedKey {
    VerifiedKey {
        base: base.to_owned(),
        secret: secret(),
        binding: DestinationBinding::key(
            ApiDestination::parse(base)
                .unwrap_or_else(|error| panic!("fixture destination refused: {error}")),
        ),
        listing,
        cloudflare: None,
        watsonx_project: None,
    }
}

#[tokio::test]
async fn reported_fields_enrich_the_row_and_the_explicit_window_is_kept(
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(serve_one(
        listener,
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"vendor/model\",\
         \"context_length\":131072,\"supported_parameters\":[\"tools\",\"temperature\"],\
         \"top_provider\":{\"max_completion_tokens\":16384},\
         \"architecture\":{\"input_modalities\":[\"text\",\"image\"],\"output_modalities\":[\"text\"]}}]}"
            .to_owned(),
    ));
    let enriched = enrich_listing(&verified(listing(), &base), "vendor/model").await;
    let request = server.await?;
    assert!(request.starts_with("GET /v1/models "));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer fixture-key"));
    let parsed: serde_json::Value = serde_json::from_slice(&enriched)?;
    let row = &parsed["data"][0];
    assert_eq!(row["id"], "vendor/model");
    assert_eq!(row["max_model_len"], 500000);
    assert_eq!(row["context_length"], 131072);
    assert_eq!(row["max_completion_tokens"], 16384);
    assert_eq!(row["supported_parameters"], json!(["tools", "temperature"]));
    assert_eq!(row["input_modalities"], json!(["text", "image"]));
    assert_eq!(row["output_modalities"], json!(["text"]));
    Ok(())
}

#[tokio::test]
async fn a_refused_or_modelless_read_leaves_the_listing_unchanged(
) -> Result<(), Box<dyn std::error::Error>> {
    for reply in [
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"other/model\",\"context_length\":1000}]}",
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nnot json",
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(serve_one(listener, reply.to_owned()));
        let unchanged = enrich_listing(&verified(listing(), &base), "vendor/model").await;
        server.await?;
        assert_eq!(unchanged, listing());
    }
    // An unreachable endpoint is also only an unchanged listing.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", closed.local_addr()?);
    drop(closed);
    assert_eq!(
        enrich_listing(&verified(listing(), &base), "vendor/model").await,
        listing()
    );
    Ok(())
}

/// A redirect is not followed: the key must not ride to another origin. The
/// reply names a second listener, which must never see a request.
#[tokio::test]
async fn a_redirect_is_not_followed_to_another_origin() -> Result<(), Box<dyn std::error::Error>> {
    let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let reply = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/v1/models\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n",
        elsewhere.local_addr()?
    );
    let server = tokio::spawn(serve_one(listener, reply));
    let unchanged = enrich_listing(&verified(listing(), &base), "vendor/model").await;
    server.await?;
    assert_eq!(unchanged, listing());
    // Nothing was ever accepted at the redirect target, so the key never moved.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), elsewhere.accept())
            .await
            .is_err(),
        "the client followed the redirect and carried the key to another origin"
    );
    Ok(())
}

#[test]
fn unreadable_fields_stay_absent_rather_than_becoming_facts() -> Result<(), String> {
    let row = json!({"id":"vendor/model","context_length":0,
        "supported_parameters":"tools",
        "top_provider":{"max_completion_tokens":-5},
        "architecture":{"input_modalities":[],"output_modalities":["text",7]}});
    assert_eq!(fields(&row), json!({}));
    assert_eq!(fields(&json!({"id":"vendor/model"})), json!({}));
    Ok(())
}

/// The read's target is the verified key's own base. There is no caller-supplied
/// destination in the signature, so a key cannot be sent anywhere else; this
/// drives enrich_listing directly and observes the only origin contacted.
#[tokio::test]
async fn the_read_goes_only_to_the_verified_base() -> Result<(), Box<dyn std::error::Error>> {
    let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(serve_one(
        listener,
        r#"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{"data":[]}"#.to_owned(),
    ));
    let enriched = enrich_listing(&verified(listing(), &base), "vendor/model").await;
    let request = server.await?;
    assert!(request.starts_with("GET /v1/models "), "{request}");
    let _ = enriched;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), elsewhere.accept())
            .await
            .is_err(),
        "a destination other than the verified base was contacted"
    );
    Ok(())
}

#[test]
fn a_row_states_its_modalities_one_way() {
    // The row's own list wins over the nested one; an empty list states nothing.
    let contradictory = json!({"output_modalities":["embedding"],
        "architecture":{"output_modalities":["text"]}});
    assert_eq!(
        fields(&contradictory)["output_modalities"],
        json!(["embedding"])
    );
    let empty_nested = json!({"output_modalities":["embedding"],
        "architecture":{"output_modalities":[]}});
    assert_eq!(
        fields(&empty_nested)["output_modalities"],
        json!(["embedding"])
    );
    let empty_own = json!({"output_modalities":[],
        "architecture":{"output_modalities":["embedding"]}});
    assert_eq!(
        fields(&empty_own)["output_modalities"],
        json!(["embedding"])
    );
}
