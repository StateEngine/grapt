//! Real configured argv regressions for lifecycle planning, without provider I/O.
use super::*;
use crate::dogfood::catalog_config::CatalogDocument;
use crate::dogfood::hooks::config::HookSettings;
use crate::dogfood::tools::shell_environment::Environment;
use grapt_types::IdAuthority;
use serde_json::json;
use std::path::PathBuf;

static CHILD_SLOT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let ids = grapt_store::OsRandomIds::seed().map_err(|_| "fixture random seed failed")?;
        let root = std::env::temp_dir().join(format!("grapt-hook-plan-{}", ids.mint().as_str()));
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    fn argv(event: Event, refuse: bool) -> Result<Vec<String>, String> {
        let event = event.as_str();
        let output_unix = if refuse {
            format!("printf '{event} fixture refused' >&2; exit 2")
        } else {
            format!(
                "text='{event}:'$count'|'; printf '%s' \"$text\"; i=${{#text}}; \
                 while [ \"$i\" -lt 4096 ]; do printf x; i=$((i+1)); done"
            )
        };
        let output_windows = if refuse {
            format!("[Console]::Error.Write('{event} fixture refused'); exit 2")
        } else {
            format!("$text='{event}:'+$count+'|'; [Console]::Out.Write($text.PadRight(4096,'x'))")
        };
        let unix = format!(
            "count=0; if [ -f '{event}-count' ]; then read -r count < '{event}-count'; fi; \
             count=$((count+1)); printf '%s' \"$count\" > '{event}-count'; \
             printf '%s\\n' '{event}' >> events; {output_unix}"
        );
        let windows = format!(
            "$count=0; if (Test-Path '{event}-count') {{ \
             $count=[int][IO.File]::ReadAllText('{event}-count') }}; $count++; \
             [IO.File]::WriteAllText('{event}-count',[string]$count); \
             [IO.File]::AppendAllText('events',\"{event}`n\"); {output_windows}"
        );
        #[cfg(windows)]
        {
            let _ = unix;
            let system = std::env::var_os("SystemRoot").ok_or("SystemRoot is missing")?;
            let executable =
                PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
            Ok(vec![
                executable.to_string_lossy().into_owned(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                windows,
            ])
        }
        #[cfg(not(windows))]
        {
            let _ = windows;
            Ok(vec!["/bin/sh".into(), "-c".into(), unix])
        }
    }

    fn plan(&self, refusal: Option<Event>) -> Result<LifecyclePlan, String> {
        // Use the documented public declaration/binding shape; no hand-built
        // HookCommand or bypass around the command allowlist/parser.
        let value = json!({
            "hook_commands": {
                "start": {"command":Self::argv(Event::SessionStart, refusal == Some(Event::SessionStart))?, "timeout_ms":10000},
                "prompt": {"command":Self::argv(Event::UserPromptSubmit, refusal == Some(Event::UserPromptSubmit))?, "timeout_ms":10000}
            },
            "hooks": {"SessionStart":["start"], "UserPromptSubmit":["prompt"]}
        });
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        let value = grapt_config::decode(&bytes, crate::dogfood::catalog_config::bounds()?)
            .map_err(|error| error.to_string())?
            .into_value();
        let settings = HookSettings::from_sources(&[CatalogDocument {
            source: self.root.join("grapt.jsonc").to_string_lossy().into_owned(),
            directory: self.root.clone(),
            origin: grapt_plugin::PluginOrigin::UserGlobal,
            value,
        }])?;
        let runtime = HookRuntime {
            settings,
            environment: Some(Environment::parent()?),
            workspace: self.root.clone(),
            notices: None,
            process_owner: None,
        };
        let session = "ses_hook_plan_fixture"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        Ok(LifecyclePlan::new(Arc::new(runtime), session))
    }

    fn events(&self) -> Result<Vec<String>, String> {
        Ok(std::fs::read_to_string(self.root.join("events"))
            .map_err(|error| error.to_string())?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn context(plan: &LifecyclePlan) -> Result<Vec<String>, String> {
    let mut input = Vec::new();
    plan.apply(&mut input)?;
    assert_eq!(input.len(), 1, "hook context should be one appended item");
    let Some(grapt_provider::CodexInputItem::Message(message)) = input.first() else {
        return Err("hook context was not a provider message".into());
    };
    assert_eq!(message.role, grapt_provider::MessageRole::User);
    let raw = message
        .content
        .strip_prefix("Lifecycle hook context (not permission grants): ")
        .ok_or("hook context label missing")?;
    serde_json::from_str(raw).map_err(|error| error.to_string())
}

fn padded(event: Event, count: usize) -> String {
    let prefix = format!("{}:{count}|", event.as_str());
    let rest = 4096 - prefix.len();
    prefix + &"x".repeat(rest)
}

#[tokio::test]
async fn promoted_prompt_context_replaces_prior_notes_while_start_context_is_retained(
) -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let mut plan = fixture.plan(None)?;
    for prompt in 1..=4 {
        if prompt > 1 {
            plan.promoted();
        }
        plan.prepare().await;
        plan.check()?;
        let notes = context(&plan)?;
        // Every event emits exactly 4 KiB: the permanent start context plus
        // the CURRENT prompt is legal at 8 KiB, even after many promotions.
        assert_eq!(
            notes,
            [
                padded(Event::SessionStart, 1),
                padded(Event::UserPromptSubmit, prompt)
            ],
            "stale prompt notes accumulated at promotion {prompt}"
        );
        let mut expected = vec!["SessionStart".to_owned()];
        expected.extend(vec!["UserPromptSubmit".to_owned(); prompt]);
        assert_eq!(fixture.events()?, expected);
        // Retries and automatic continuation call preparation again but do not
        // promote new input. Neither hook may repeat, nor can notes grow.
        for _ in 0..3 {
            plan.prepare().await;
            plan.check()?;
            assert_eq!(context(&plan)?, notes);
            assert_eq!(fixture.events()?, expected, "preparation repeated a hook");
        }
    }
    Ok(())
}

#[tokio::test]
async fn startup_and_prompt_refusals_latch_and_never_apply_partial_context() -> Result<(), String> {
    let _slot = CHILD_SLOT.lock().await;
    for refused in [Event::SessionStart, Event::UserPromptSubmit] {
        let fixture = Fixture::new()?;
        let mut plan = fixture.plan(Some(refused))?;
        plan.prepare().await;
        let reason = plan.check().err().ok_or("hook refusal was not latched")?;
        assert!(
            reason.contains(&format!("{} fixture refused", refused.as_str())),
            "{reason}"
        );
        let expected = match refused {
            Event::SessionStart => vec!["SessionStart".to_owned()],
            _ => vec!["SessionStart".to_owned(), "UserPromptSubmit".to_owned()],
        };
        assert_eq!(fixture.events()?, expected);
        let original = grapt_provider::CodexInputItem::Message(grapt_provider::InputMessage {
            role: grapt_provider::MessageRole::User,
            content: "original provider input".to_owned(),
        });
        let mut input = vec![original];
        let before = input.clone();
        assert_eq!(plan.apply(&mut input), Err(reason.clone()));
        assert_eq!(input, before, "failed preparation appended partial context");
        for _ in 0..2 {
            // Even a redundant promotion must not resurrect an already refused
            // response chain and run a side-effecting hook a second time.
            plan.promoted();
            plan.prepare().await;
            assert_eq!(plan.check(), Err(reason.clone()));
            assert_eq!(fixture.events()?, expected);
        }
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_configured_hook_cannot_be_skipped_by_preparation_retry() -> Result<(), String>
{
    use std::future::Future;
    use std::task::Poll;
    let _slot = CHILD_SLOT.lock().await;
    let fixture = Fixture::new()?;
    let mut plan = fixture.plan(None)?;
    {
        let mut pending = std::pin::pin!(plan.prepare());
        let interrupted =
            std::future::poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx).is_pending())).await;
        assert!(
            interrupted,
            "configured hook must enter asynchronous execution"
        );
    }
    let reason = plan
        .check()
        .err()
        .ok_or("cancelled hook was treated as complete")?;
    assert!(
        reason.contains("did not complete before cancellation or deadline"),
        "{reason}"
    );
    plan.prepare().await;
    assert_eq!(plan.check(), Err(reason.clone()));
    assert_eq!(plan.apply(&mut Vec::new()), Err(reason));
    Ok(())
}
