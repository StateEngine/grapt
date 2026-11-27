//! Product preparation uses actual native HTTP ownership through the Step driver.
use super::*;
use crate::dogfood::native_mail::tests::http::serve_preparation_retry;
use crate::dogfood::production::{tests::projection_targets, ProductPlan};
use grapt_provider::{
    deterministic::ScriptBuilder, AttemptOpener, CredentialPlacement, FinishReason,
    OpenerCredential, PromptCacheKey, ProtocolFamily,
};
use grapt_runtime::{
    provider_source::ProviderAttemptSource, step_interrupt, AttemptSource, NoCompaction,
    RecordingOwner, StepDriver, StepPolicy, StepResult, StepTerminalHandoff,
};
use grapt_types::{PolicyVersion, Secret};
use std::{num::NonZeroU32, time::Duration};

impl grapt_runtime::Clock for Clock {
    fn now_ms(&self) -> i64 {
        1760000000123
    }
}

struct NoTools;
impl grapt_runtime::ToolRunner for NoTools {
    fn run(
        &self,
        _call: grapt_types::ToolCallId,
        _tool: String,
        _input: grapt_runtime::BoundedToolInput,
    ) -> grapt_runtime::BoxFuture<'static, grapt_runtime::ToolOutcome> {
        panic!("preparation regression must not invoke tools")
    }
}

fn source(
    inbox: Arc<Mutex<Inbox>>,
    session: SessionId,
    mail: NativeMailPlan,
) -> Result<ProviderAttemptSource<ProductPlan>, Box<dyn std::error::Error>> {
    let step = StepNumber::new(1).ok_or("step")?;
    let targets = projection_targets(ProtocolFamily::ChatGptCodex)?;
    let target = targets.issue()?;
    let credential = OpenerCredential::inject(
        target.account().clone(),
        target.credential_version().clone(),
        CredentialPlacement::BearerAuthorization,
        Secret::new("obviously-fake-provider-token".to_owned()),
    )?;
    let opener = AttemptOpener::deterministic(
        [ScriptBuilder::new()
            .text(&["prepared"])
            .finish(FinishReason::Stop, None)],
        target.route().clone(),
        credential,
    );
    let plan = ProductPlan::transient(
        ProtocolFamily::ChatGptCodex,
        "current task".into(),
        PromptCacheKey::for_session(&session)?,
        target,
        targets,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        Vec::new(),
        step,
    )
    .with_mail_owner(mail);
    let mut source = ProviderAttemptSource::new(
        Arc::new(opener),
        inbox,
        Arc::new(grapt_store::OsRandomIds::seed()?),
        Arc::new(Clock),
        session,
        grapt_store::AttemptRecordCeiling::new(NonZeroU32::new(512).ok_or("ceiling")?),
        plan,
    );
    source.begin_step(StepTerminalHandoff::new(
        step,
        "evt_preparation_terminal".parse()?,
        1760000000123,
    ));
    Ok(source)
}

async fn run(
    source: &mut ProviderAttemptSource<ProductPlan>,
    attempts: u32,
) -> Result<grapt_runtime::StepReport, Box<dyn std::error::Error>> {
    let policy = StepPolicy::new(
        PolicyVersion::new("native-preparation-test/1").ok_or("policy")?,
        attempts,
        Duration::from_millis(1),
        0,
        Duration::from_millis(250),
    )?;
    let (_interrupt, cancellation) = step_interrupt();
    let (_, report) = StepDriver::run(
        StepNumber::new(1).ok_or("step")?,
        policy,
        source,
        &mut NoCompaction,
        Arc::new(NoTools),
        RecordingOwner::new(),
        cancellation,
    )
    .await;
    Ok(report)
}

#[tokio::test]
async fn product_retries_preparation_after_confirmed_native_close_and_succeeds() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    context.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let mut source = source(
        context.inbox.clone(),
        context.session.clone(),
        NativeMailPlan::private(context),
    )?;
    let (server, client) = tokio::join!(serve_preparation_retry(listener), async {
        let result = run(&mut source, 2).await;
        let closed = source.shutdown().await;
        let report = result?;
        closed?;
        assert_eq!(
            report.result,
            StepResult::Ended {
                finish: FinishReason::Stop
            }
        );
        assert_eq!(
            report.receipts.len(),
            1,
            "preparation timeout bought a provider attempt"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    server?;
    client
}
