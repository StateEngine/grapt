//! Exercise the production Basic-authenticated HTTP host, not a bare router.
use super::*;
use grapt_runtime::SessionPorts;
use grapt_store::EventStore;
use grapt_types::DeterministicAuthorities;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Root(std::path::PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

async fn call(
    application: &Arc<DogfoodApplication>,
    path: &str,
    body: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let limits = crate::compose_host::host_limits()?;
    let config = crate::input::ServiceConfig {
        slot: "product".into(),
        instance: "image-ingress".into(),
        environment: "acceptance".into(),
        build: "image-ingress".into(),
        version: "0.1.0".into(),
        serve_window_ms: 10000,
        provider_connect_ms: 1000,
        provider_response_head_ms: 1000,
        provider_active_attempts: 1,
        close_grace_ms: None,
        max_inflight_streams: None,
    };
    let service = crate::compose_host::product_service(
        &limits,
        &crate::input::Credential {
            username: "fixture".into(),
            secret: "secret".into(),
        },
        grapt_types::service::ProcessId::new(std::process::id()).ok_or("process")?,
        &config,
        Some(crate::compose::DogfoodComposition::ready(
            application.clone(),
            application.clone(),
            application.hub.clone(),
            None,
        )),
    )?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback().await?,
        service,
        limits,
        Arc::new(grapt_api::TokioDeadlineDriver::new()),
    )?
    .start();
    let result = async {
        let mut socket = tokio::net::TcpStream::connect(host.ready().observed_address()).await?;
        let head = format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic Zml4dHVyZTpzZWNyZXQ=\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(body.as_bytes()).await?;
        let mut response = Vec::new();
        for _ in 0..64 {
            let mut chunk = [0; 2048];
            let count = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut chunk)).await??;
            if count == 0 { break; }
            response.extend_from_slice(&chunk[..count]);
        }
        Ok::<_, Box<dyn std::error::Error>>(response)
    }.await;
    assert!(
        matches!(
            host.stop().await,
            grapt_api::ApiHostExit::StoppedGracefully { .. }
        ),
        "image host did not join"
    );
    result
}

#[tokio::test]
async fn inline_images_cross_authenticated_ingress_without_widening_ordinary_requests() -> TestResult
{
    let root =
        Root(std::env::temp_dir().join(format!("grapt-image-ingress-{}", std::process::id())));
    std::fs::create_dir(&root.0)?;
    let (application, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        EventStore::open_in_memory()?,
        &crate::dogfood::production::tests::projection_target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_image_ingress").ok_or("project")?,
        root.0.to_str().ok_or("directory")?,
        "image ingress",
    )?;
    let session = SessionPorts::new(application.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    // Deterministic, poorly compressible pixels make a real PNG larger than the ordinary limit.
    let mut state = 1_u32;
    let pixels = ::image::RgbImage::from_fn(192, 192, |_, _| {
        let mut rgb = [0; 3];
        for channel in &mut rgb {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *channel = state as u8;
        }
        ::image::Rgb(rgb)
    });
    let mut png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgb8(pixels).write_to(&mut png, ::image::ImageFormat::Png)?;
    let body = json!({"id":"msg_ingress_image", "text":"describe", "resume":false,
        "files":[{"uri":format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png.into_inner()))}]}).to_string();
    assert!(body.len() > 64 * 1024);
    let response = call(
        &application,
        &format!("/api/session/{session}/prompt"),
        &body,
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "image prompt was refused: {}",
        String::from_utf8_lossy(&response)
    );
    {
        let inbox = application.lock_inbox();
        let pending = inbox.pending_inputs(&session)?;
        assert_eq!(pending.len(), 1);
        let refs = grapt_inbox::user_image_references(pending[0].metadata.as_ref())?;
        assert_eq!(refs.len(), 1, "HTTP admission lost pixels");
    }
    let ordinary = json!({"text":"x".repeat(65536)}).to_string();
    let response = call(
        &application,
        &format!("/api/session/{session}/prompt"),
        &ordinary,
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 400"),
        "oversized text prompt was admitted"
    );
    let response = call(&application, "/api/session", &ordinary).await?;
    assert!(
        response.starts_with(b"HTTP/1.1 413"),
        "ordinary ingress limit changed"
    );
    assert_eq!(application.lock_inbox().pending_inputs(&session)?.len(), 1);
    Ok(())
}
