//! Endpoint selection uses the same verified Session file as MCP discovery.
use super::*;
use crate::dogfood::native_mail::tests::{context_at, http::serve};
use grapt_instruction::InstructionService;
use grapt_session::config_binding::{self, ConfigBinding};
use grapt_types::{DeterministicAuthorities, IdAuthority, IdToken, InstructionKey, StepNumber};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::net::TcpListener;

type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Ids(AtomicU64);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("{:016x}", self.0.fetch_add(1, Ordering::Relaxed)))
            .unwrap_or_else(|| panic!("fixture id"))
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = Self(std::env::temp_dir().join(format!(
            "grapt-session-mail-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::create_dir(&root.0)?;
        grapt_login::profile_security::protect_directory(&root.0)?;
        std::fs::create_dir(root.0.join("service"))?;
        std::fs::create_dir(root.0.join("project"))?;
        Ok(root)
    }
    fn config(
        &self,
        name: &str,
        session: &SessionId,
        endpoint: &str,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = self.0.join(name);
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(&serde_json::to_vec(&json!({
            "stateengine":{"native_mail_endpoint":endpoint},
            "mcp":{"stateengine":{"type":"local","enabled":false,"command":["node","fixture.mjs"],
                "environment":{"STATEENGINE_PROJECT_ID":"secode","STATEENGINE_AGENT_ID":"coder",
                    "SECODE_SESSION_ID":session.as_str()}}}
        }))?)?;
        grapt_login::profile_security::protect_new_private_file(&path, &file)?;
        Ok(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}
fn bind(context: &crate::dogfood::native_mail::NativeMailContext, path: &Path) -> TestResult {
    let authorities = DeterministicAuthorities::new(&*context.clock, &*context.ids);
    config_binding::bind(
        context.inbox.lock().map_err(|_| "inbox")?.store_mut(),
        &authorities,
        &context.session,
        ConfigBinding {
            path: path.to_str().ok_or("path")?.to_owned(),
            project: grapt_types::remote_mail::SourceProjectId::try_new("secode")?,
            agent: grapt_types::remote_mail::SourcePrincipalId::try_new("coder")?,
        },
    )?;
    Ok(())
}
async fn roundtrip(
    context: &mut crate::dogfood::native_mail::NativeMailContext,
    listener: TcpListener,
) -> TestResult {
    let (server, client) = tokio::join!(serve(listener, None), async {
        let result = context
            .refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|error| error.to_string());
        let batch = if result.is_ok() {
            context.take_batch().map_err(|error| error.to_string())
        } else {
            Ok(None)
        };
        let closed = context.finish().await;
        result?;
        assert!(batch?.is_some(), "selected endpoint supplied no mail");
        closed?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    server?;
    client
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn bound_endpoint_wins_and_retained_context_uses_the_rebound_file() -> TestResult {
    let root = Root::new()?;
    let first = TcpListener::bind("127.0.0.1:0").await?;
    let other = TcpListener::bind("127.0.0.1:0").await?;
    let service = root.0.join("service");
    std::fs::write(
        service.join("grapt.jsonc"),
        serde_json::to_vec(&json!({
            "stateengine":{"native_mail_endpoint":format!("http://127.0.0.1:{}",other.local_addr()?.port())}
        }))?,
    )?;
    let mut context = context_at(
        Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder"}"#),
        true,
        root.0.join("project").to_str().ok_or("project path")?,
    )?;
    context.ids = Arc::new(Ids(AtomicU64::new(1000)));
    context.service_root = Some(service.clone());
    let config = root.config(
        "first.json",
        &context.session,
        &format!("http://127.0.0.1:{}", first.local_addr()?.port()),
    )?;
    bind(&context, &config)?;
    roundtrip(&mut context, first).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), other.accept())
            .await
            .is_err(),
        "private credentials reached the service-wide endpoint instead of the Session endpoint"
    );

    // Reuse the exact context retained by production's owner. A new transport
    // must select the new file, even though project and agent are unchanged.
    let second = TcpListener::bind("127.0.0.1:0").await?;
    let config = root.config(
        "second.json",
        &context.session,
        &format!("http://127.0.0.1:{}", second.local_addr()?.port()),
    )?;
    bind(&context, &config)?;
    roundtrip(&mut context, second).await?;

    let other_session = {
        let authorities = DeterministicAuthorities::new(&*context.clock, &*context.ids);
        let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
        let seed = grapt_session::SessionSeed::new(
            grapt_types::ProjectId::new("prj_other").ok_or("project")?,
            context.location.directory(),
            "other Session",
        )?;
        let other_session =
            grapt_session::create_session(inbox.store_mut(), &authorities, &seed, &mut [])?.session;
        InstructionService::put_entry(
            inbox.store_mut(),
            &other_session,
            &InstructionKey::new("stateengine-identity").ok_or("identity")?,
            r#"{"scope":"project","project_id":"secode","agent_id":"other"}"#,
            1,
        )?;
        other_session
    };
    assert_eq!(
        read(
            &context.inbox,
            &other_session,
            Path::new(context.location.directory()),
            &service
        )
        .map_err(|error| error.to_string())?
        .get(),
        other.local_addr()?.port(),
        "an unbound Session inherited the private file"
    );

    // An explicit private file must supply its own endpoint, even when a
    // valid service-wide endpoint exists. Missing fields are not inheritance.
    for field in ["stateengine", "native_mail_endpoint"] {
        let selected = root.config("second.json", &context.session, "http://127.0.0.1:49321")?;
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&selected)?)?;
        let object = if field == "stateengine" {
            value.as_object_mut().ok_or("fixture config object")?
        } else {
            value["stateengine"]
                .as_object_mut()
                .ok_or("fixture endpoint object")?
        };
        assert!(
            object.remove(field).is_some(),
            "missing-field fixture was unchanged"
        );
        std::fs::write(&selected, serde_json::to_vec(&value)?)?;
        assert_eq!(
            read(
                &context.inbox,
                &context.session,
                Path::new(context.location.directory()),
                &service
            ),
            Err(Refusal::Missing),
            "selected private file inherited the service endpoint after losing {field}"
        );
        assert!(context
            .refresh(StepNumber::new(2).ok_or("step")?)
            .await
            .is_err());
        assert!(context.runtime.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), other.accept())
                .await
                .is_err(),
            "missing {field} sent private credentials to the service endpoint"
        );
    }

    // Invalid selected declarations fail closed; the valid service fallback
    // must not receive a credential after a damaged or malicious file edit.
    let bad = root.config("second.json", &context.session, "http://192.0.2.1:49321")?;
    assert_eq!(
        read(
            &context.inbox,
            &context.session,
            Path::new(context.location.directory()),
            &service
        ),
        Err(Refusal::Invalid)
    );
    assert!(context
        .refresh(StepNumber::new(2).ok_or("step")?)
        .await
        .is_err());
    assert!(context.runtime.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), other.accept())
            .await
            .is_err()
    );
    std::fs::remove_file(bad)?;
    assert_eq!(
        read(
            &context.inbox,
            &context.session,
            Path::new(context.location.directory()),
            &service
        ),
        Err(Refusal::Unreadable)
    );
    Ok(())
}
