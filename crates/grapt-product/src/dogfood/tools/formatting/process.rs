//! Configured argv execution through the shared bounded command owner.
use super::super::shell_environment::Environment;
use super::config::Entry;
use super::*;
use std::io::{Read, Write};

pub(super) async fn format(
    context: &Context<'_>,
    relative: &Path,
    before: &[u8],
    entry: &Entry,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, String> {
    // Own the entire stage, including its temporary file, beyond caller drop.
    let stage = Stage {
        root: context.root.into(),
        service: context.service.into(),
        credentials: context.credentials.to_vec(),
        relative: relative.into(),
        before: before.to_vec(),
        entry: entry.clone(),
        cancellation: context.cancellation.clone(),
        deadline,
    };
    match context.process_owner {
        Some((terminals, session)) => {
            terminals
                .run_helper_owned(session.clone(), move |control| async move {
                    stage.run(Some(&control)).await
                })
                .await
        }
        None => stage.run(None).await,
    }
}

struct Stage {
    root: PathBuf,
    service: PathBuf,
    credentials: Vec<String>,
    relative: PathBuf,
    before: Vec<u8>,
    entry: Entry,
    cancellation: ToolCancellation,
    deadline: tokio::time::Instant,
}

impl Stage {
    async fn run(
        &self,
        control: Option<&crate::dogfood::tools::ShellControl>,
    ) -> Result<Vec<u8>, String> {
        let Self {
            root,
            service,
            credentials,
            relative,
            before,
            entry,
            cancellation,
            deadline,
        } = self;
        std::str::from_utf8(before).map_err(|_| "formatter input is not UTF-8")?;
        if entry.command.is_empty() {
            return Err("formatter has no command; configure command argv in user settings".into());
        }
        let suffix = relative
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{s}"))
            .unwrap_or_default();
        let parent = relative.parent().ok_or("formatter target has no parent")?;
        let directory = resolve_existing(
            root,
            parent.to_str().ok_or("formatter parent is not UTF-8")?,
            true,
        )?;
        let mut copy = tempfile::Builder::new()
            .prefix(".grapt-format-")
            .suffix(&suffix)
            .tempfile_in(&directory)
            .map_err(|_| "formatter sibling staging failed; edit bytes retained")?;
        copy.write_all(before)
            .and_then(|()| copy.flush())
            .map_err(|_| "formatter staging write failed")?;
        let environment =
            Environment::for_configured_command(service, root, credentials, &entry.environment)?;
        let program = super::super::configured_program::resolve(
            &entry.command[0],
            root,
            &environment,
            "formatter",
        )?;
        let file = copy
            .path()
            .to_str()
            .ok_or("formatter staging path is not UTF-8")?;
        let mut argv = vec![program
            .to_str()
            .ok_or("formatter executable is not UTF-8")?
            .into()];
        argv.extend(
            entry
                .command
                .iter()
                .skip(1)
                .map(|s| s.replace("$FILE", file)),
        );
        if !entry.command.iter().skip(1).any(|s| s.contains("$FILE")) {
            argv.push(file.into());
        }
        let invoked_as =
            (!Path::new(&entry.command[0]).is_absolute()).then_some(entry.command[0].as_str());
        let result = crate::dogfood::hooks::ConfiguredCommand {
            argv: &argv,
            directory: root,
            environment: &environment,
            input: &[],
            timeout_ms: 10_000,
            plugin: None,
            invoked_as,
        }
        .run(crate::dogfood::hooks::helper::Control {
            owner: control,
            cancellation: Some(cancellation),
            deadline: (*deadline).min(tokio::time::Instant::now() + Duration::from_secs(10)),
        })
        .await?;
        if !result.0.success() {
            return Err(match result.0.code() {
                Some(code) => format!("formatter exited with code {code}; edit retained"),
                None => "formatter terminated without an exit code; edit retained".into(),
            });
        }
        // Reopen with no-follow/nonblocking checks: formatter output may be replaced,
        // but a FIFO, link or reparse point must never become a read or publication.
        let mut after = Vec::new();
        coding::patch::open_stage(copy.path())
            .map_err(|_| "formatter output is not a readable regular file")?
            .take(FILE_EFFECT_MAX_BYTES + 1)
            .read_to_end(&mut after)
            .map_err(|_| "formatter output read failed")?;
        if after.len() as u64 > FILE_EFFECT_MAX_BYTES {
            return Err("formatter output exceeds 1 MiB; edit retained".into());
        }
        std::str::from_utf8(&after).map_err(|_| "formatter output is not UTF-8; edit retained")?;
        copy.close()
            .map_err(|_| "formatter temporary file cleanup failed; edit bytes retained")?;
        Ok(after)
    }
}
