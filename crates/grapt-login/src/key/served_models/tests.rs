//! A served listing keeps only windows a server states it serves.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A loopback server that answers each path from `routes` and 404 otherwise,
/// recording every requested path. A route whose body is `HANG` never
/// answers. Each connection is served on its own task.
async fn local_server(
    routes: &'static [(&'static str, &'static str)],
) -> Result<(String, tokio::sync::mpsc::Receiver<String>), std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let (seen, paths) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(answer(socket, routes, seen.clone()));
        }
    });
    Ok((base, paths))
}

async fn answer(
    mut socket: tokio::net::TcpStream,
    routes: &'static [(&'static str, &'static str)],
    seen: tokio::sync::mpsc::Sender<String>,
) {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") && head.len() <= 8192 {
        let Ok(byte) = socket.read_u8().await else {
            return;
        };
        head.push(byte);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
    let route = routes.iter().find(|(route, _)| *route == path);
    if seen.send(path).await.is_err() {
        return;
    }
    let reply = match route {
        Some((_, "HANG")) => {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            return;
        }
        Some((_, body)) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_owned(),
    };
    let _ = socket.write_all(reply.as_bytes()).await;
}

fn verified(listing: &str, base: &str) -> Result<VerifiedKey, Box<dyn std::error::Error>> {
    Ok(VerifiedKey {
        base: base.to_owned(),
        secret: VaultSecret::api_key("local".to_owned().into())?,
        binding: DestinationBinding::key(ApiDestination::parse(base)?),
        listing: listing.as_bytes().to_vec(),
        cloudflare: None,
        watsonx_project: None,
    })
}

fn served_rows(listing: &[u8]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let parsed: Value = serde_json::from_slice(listing)?;
    Ok(parsed["data"].as_array().cloned().unwrap_or_default())
}

#[tokio::test]
async fn every_listed_model_is_offered_with_the_window_the_listing_serves() -> TestResult {
    let (base, _paths) = local_server(&[]).await?;
    let verified = verified(
        r#"{"data":[{"id":"small","max_model_len":65536},{"id":"large","max_model_len":131072}]}"#,
        &base,
    )?;
    assert_eq!(
        candidate_models(verified.listing(), |_| true),
        ["small", "large"]
    );
    assert_eq!(
        candidate_models(verified.listing(), |row| row["id"] != "small"),
        ["large"]
    );
    let listing = served_listing(&verified, "large").await?;
    let rows = served_rows(&listing)?;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]["id"], "large",
        "the checked model leads the listing"
    );
    assert_eq!(rows[0]["max_model_len"], 131072);
    assert_eq!(rows[1]["id"], "small");
    assert_eq!(rows[1]["max_model_len"], 65536);
    Ok(())
}

#[tokio::test]
async fn a_loaded_model_takes_its_served_window_and_an_unloaded_one_stays_unknown() -> TestResult {
    let (base, _paths) = local_server(&[(
        "/api/ps",
        r#"{"models":[{"name":"qwen:latest","model":"qwen:latest","context_length":65536}]}"#,
    )])
    .await?;
    let verified = verified(
        r#"{"data":[{"id":"qwen:latest"},{"id":"llama:latest"}]}"#,
        &base,
    )?;
    let listing = served_listing(&verified, "qwen:latest").await?;
    let rows = served_rows(&listing)?;
    assert_eq!(rows[0]["id"], "qwen:latest");
    assert_eq!(rows[0]["max_model_len"], 65536);
    assert_eq!(rows[1]["id"], "llama:latest");
    assert!(
        rows[1].get("max_model_len").is_none(),
        "an unloaded model's window was invented"
    );
    Ok(())
}

#[tokio::test]
async fn a_loaded_lm_studio_model_and_a_single_llama_cpp_model_report_their_windows() -> TestResult
{
    let (base, _paths) = local_server(&[(
        "/api/v0/models",
        r#"{"data":[{"id":"phi","state":"loaded","max_context_length":131072,"loaded_context_length":32768},{"id":"gemma","state":"not-loaded","max_context_length":131072}]}"#,
    )])
    .await?;
    let listing = served_listing(
        &verified(r#"{"data":[{"id":"phi"},{"id":"gemma"}]}"#, &base)?,
        "phi",
    )
    .await?;
    let rows = served_rows(&listing)?;
    assert_eq!(
        rows[0]["max_model_len"], 32768,
        "the loaded window, not the maximum"
    );
    assert!(rows[1].get("max_model_len").is_none());

    let (base, _paths) = local_server(&[(
        "/props",
        r#"{"default_generation_settings":{"n_ctx":65536},"total_slots":1}"#,
    )])
    .await?;
    let listing = served_listing(
        &verified(
            r#"{"data":[{"id":"model.gguf","meta":{"n_ctx_train":262144}}]}"#,
            &base,
        )?,
        "model.gguf",
    )
    .await?;
    assert_eq!(served_rows(&listing)?[0]["max_model_len"], 65536);
    Ok(())
}

#[tokio::test]
async fn a_chosen_model_without_a_served_window_or_listing_is_refused() -> TestResult {
    let (base, _paths) = local_server(&[]).await?;
    let verified = verified(r#"{"data":[{"id":"qwen:latest"}]}"#, &base)?;
    assert_eq!(
        served_listing(&verified, "qwen:latest").await,
        Err(ServedError::NoWindow)
    );
    assert_eq!(
        served_listing(&verified, "absent").await,
        Err(ServedError::NotServed)
    );
    Ok(())
}

#[tokio::test]
async fn a_remote_endpoint_gets_no_server_specific_reads() -> TestResult {
    let verified = verified(
        r#"{"data":[{"id":"hosted"}]}"#,
        "https://models.example.invalid/v1",
    )?;
    // Without a stated window the chosen model is refused, and nothing was
    // asked of the remote origin beyond the listing it already returned.
    assert_eq!(
        served_listing(&verified, "hosted").await,
        Err(ServedError::NoWindow)
    );
    assert_eq!(loopback_origin("https://models.example.invalid/v1"), None);
    assert_eq!(loopback_origin("http://localhost:11434/v1"), None);
    assert_eq!(
        loopback_origin("http://[::1]:11434/v1").as_deref(),
        Some("http://[::1]:11434")
    );
    Ok(())
}

#[tokio::test]
async fn a_loaded_lm_studio_instance_reports_its_window_through_the_v1_list() -> TestResult {
    let (base, _paths) = local_server(&[(
        "/api/v1/models",
        r#"{"models":[{"type":"llm","key":"google/gemma","loaded_instances":[{"id":"google/gemma","config":{"context_length":65536}}],"max_context_length":262144},{"type":"llm","key":"phi","loaded_instances":[],"max_context_length":131072}]}"#,
    )])
    .await?;
    let listing = served_listing(
        &verified(r#"{"data":[{"id":"google/gemma"},{"id":"phi"}]}"#, &base)?,
        "google/gemma",
    )
    .await?;
    let rows = served_rows(&listing)?;
    assert_eq!(
        rows[0]["max_model_len"], 65536,
        "the loaded window, not the maximum"
    );
    assert!(rows[1].get("max_model_len").is_none());
    Ok(())
}

#[tokio::test]
async fn server_reads_that_never_answer_hold_a_connect_only_briefly() -> TestResult {
    let (base, _paths) = local_server(&[
        ("/api/ps", "HANG"),
        ("/api/v1/models", "HANG"),
        ("/api/v0/models", "HANG"),
        ("/props", "HANG"),
    ])
    .await?;
    let verified = verified(r#"{"data":[{"id":"only"}]}"#, &base)?;
    // Four silent reads, each bounded and run together: well under the sum.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        served_listing(&verified, "only"),
    )
    .await?;
    assert_eq!(outcome, Err(ServedError::NoWindow));
    Ok(())
}

#[tokio::test]
async fn a_served_row_keeps_its_live_output_and_each_reasoning_level_once() -> TestResult {
    let verified = verified(
        r#"{"data":[{"id":"qwen","max_model_len":65536,"output_modalities":["text"],"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"low"}]}]}"#,
        "https://models.example.invalid/v1",
    )?;
    let rows = served_rows(&served_listing(&verified, "qwen").await?)?;
    assert_eq!(rows[0]["output_modalities"], serde_json::json!(["text"]));
    assert_eq!(
        rows[0]["supported_reasoning_levels"],
        serde_json::json!([{"effort":"low"},{"effort":"high"}])
    );
    Ok(())
}
