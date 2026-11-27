use super::*;
use crate::dogfood::catalog_config::{CatalogEnvironment, CatalogSources};
use grapt_plugin::PluginOrigin;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn actual_catalog_sources_keep_repository_redirects_below_user_configuration() -> TestResult {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let root = std::env::temp_dir().join(format!(
        "grapt-mail-endpoint-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&root)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let service = root.join("service");
    let repository = root.join("repository");
    std::fs::create_dir(&service)?;
    std::fs::create_dir(&repository)?;
    std::fs::create_dir(repository.join(".git"))?;
    std::fs::write(
        service.join("grapt.jsonc"),
        r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:49321"}}"#,
    )?;
    std::fs::write(
        repository.join("grapt.jsonc"),
        r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:49322"}}"#,
    )?;
    let sources = CatalogSources::read(&repository, CatalogEnvironment::isolated(&root))?;
    assert_eq!(
        configured(&sources.documents).map(NonZeroU16::get),
        Ok(49321)
    );
    std::fs::remove_file(service.join("grapt.jsonc"))?;
    let sources = CatalogSources::read(&repository, CatalogEnvironment::isolated(&root))?;
    assert_eq!(configured(&sources.documents), Err(Refusal::Missing));
    Ok(())
}

fn document(
    origin: PluginOrigin,
    raw: &str,
) -> Result<CatalogDocument, Box<dyn std::error::Error>> {
    Ok(CatalogDocument {
        directory: "/fixture".into(),
        source: "fixture.jsonc".into(),
        origin,
        value: grapt_config::decode(raw.as_bytes(), crate::dogfood::catalog_config::bounds()?)?
            .into_value(),
    })
}

#[test]
fn only_trusted_configuration_selects_the_loopback_endpoint() -> TestResult {
    let user = || {
        document(
            PluginOrigin::UserGlobal,
            r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:49321"}}"#,
        )
    };
    let project = || {
        document(
            PluginOrigin::Project,
            r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:49322"}}"#,
        )
    };
    assert_eq!(configured(&[]), Err(Refusal::Missing));
    assert_eq!(configured(&[project()?]), Err(Refusal::Missing));
    assert_eq!(
        configured(&[user()?, project()?]).map(NonZeroU16::get),
        Ok(49321)
    );
    let explicit = document(
        PluginOrigin::LaunchExplicit,
        r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:49323/"}}"#,
    )?;
    assert_eq!(
        configured(&[user()?, explicit]).map(NonZeroU16::get),
        Ok(49323)
    );
    for raw in [
        r#"{"stateengine":null}"#,
        r#"{"stateengine":{"native_mail_endpoint":4877}}"#,
        r#"{"stateengine":{"native_mail_endpoint":"http://127.0.0.1:65536"}}"#,
    ] {
        assert_eq!(
            configured(&[document(PluginOrigin::UserGlobal, raw)?]),
            Err(Refusal::Invalid)
        );
    }
    Ok(())
}

#[test]
fn declared_endpoint_never_widens_the_sealed_credential_destination_or_falls_back() {
    for invalid in [
        "",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:49321/path",
        "http://127.0.0.1:49321?secret=private",
        "http://private@127.0.0.1:49321",
        "http://localhost:49321",
        "http://192.0.2.1:49321",
        "https://127.0.0.1:49321",
        "http://127.0.0.1:49321#private",
    ] {
        assert_eq!(parse(invalid), Err(Refusal::Invalid));
        assert!(!Refusal::Invalid.to_string().contains("private"));
    }
}

#[tokio::test]
async fn a_configured_non_default_port_receives_the_real_authenticated_mail_flow() -> TestResult {
    use crate::dogfood::native_mail::tests::{context, http::serve};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    assert_ne!(
        port, 4877,
        "probe must exercise a declared non-default port"
    );
    let config =
        format!(r#"{{"stateengine":{{"native_mail_endpoint":"http://127.0.0.1:{port}"}}}}"#);
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.port = configured(&[document(PluginOrigin::LaunchExplicit, &config)?]);
    let (server, client) = tokio::join!(serve(listener, None), async {
        context
            .refresh(grapt_types::StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|e| e.to_string())?;
        assert!(context.runtime.is_some());
        assert!(context.take_batch().map_err(|e| e.to_string())?.is_some());
        context.finish().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    server?;
    client?;
    Ok(())
}

#[tokio::test]
async fn an_invalid_declared_endpoint_refuses_before_opening_any_transport() -> TestResult {
    let mut context = crate::dogfood::native_mail::tests::context(
        Some(r#"{"project_id":"secode","agent_id":"coder"}"#),
        true,
    )?;
    context.port = configured(&[document(
        PluginOrigin::UserGlobal,
        r#"{"stateengine":{"native_mail_endpoint":"http://192.0.2.1:49321"}}"#,
    )?]);
    assert!(matches!(
        context
            .refresh(grapt_types::StepNumber::new(1).ok_or("step")?)
            .await,
        Err(crate::dogfood::stateengine_resume::ContinuityUnavailable::Transport)
    ));
    assert!(context.runtime.is_none());
    context.finish().await?;
    Ok(())
}
