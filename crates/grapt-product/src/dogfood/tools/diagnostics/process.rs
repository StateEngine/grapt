//! One contained language-server exchange, retained by its session process owner.
use super::*;
use crate::dogfood::tools::shell_environment::Environment;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) async fn collect(
    context: &Context<'_>,
    entry: &config::Entry,
    documents: &[Document],
    deadline: tokio::time::Instant,
) -> Result<Vec<Value>, String> {
    let stage = Stage {
        root: context.root.into(),
        service: context.service.into(),
        credentials: context.credentials.to_vec(),
        entry: entry.clone(),
        documents: documents.to_vec(),
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
    entry: config::Entry,
    documents: Vec<Document>,
    cancellation: ToolCancellation,
    deadline: tokio::time::Instant,
}

impl Stage {
    async fn run(
        &self,
        control: Option<&crate::dogfood::tools::ShellControl>,
    ) -> Result<Vec<Value>, String> {
        let Self {
            root,
            service,
            credentials,
            entry,
            documents,
            cancellation,
            deadline,
        } = self;
        let environment =
            Environment::for_configured_command(service, root, credentials, &entry.environment)?;
        let (program, arguments) = entry
            .command
            .split_first()
            .ok_or("LSP command is absent; configure command argv in user settings")?;
        let image = super::super::configured_program::resolve(program, root, &environment, "LSP")?;
        let mut command = tokio::process::Command::new(image);
        #[cfg(unix)]
        if !Path::new(program).is_absolute() {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().arg0(program);
        }
        command
            .args(arguments)
            .current_dir(root)
            .env_clear()
            .envs(environment.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        grapt_service::launcher_environment::scrub(command.as_std_mut());
        let (status, results) = crate::dogfood::hooks::helper::run(
            command,
            crate::dogfood::hooks::helper::Control {
                owner: control,
                cancellation: Some(cancellation),
                deadline: *deadline,
            },
            |mut stdin, stdout, stderr| async move {
                let (results, ()) = tokio::try_join!(
                    protocol::collect(&mut stdin, stdout, root, &entry.initialization, documents),
                    drain(stderr),
                )?;
                Ok::<_, String>(results)
            },
        )
        .await?;
        if !status.success() {
            return Err(format!(
                "LSP exited unsuccessfully after diagnostics (code {:?}); edit retained",
                status.code()
            ));
        }
        Ok(results)
    }
}

async fn drain(mut stream: impl AsyncRead + Unpin) -> Result<(), String> {
    let mut buffer = [0; 4096];
    let mut total = 0;
    loop {
        let count = stream
            .read(&mut buffer)
            .await
            .map_err(|_| "LSP stderr read failed")?;
        if count == 0 {
            return Ok(());
        }
        total += count;
        if total > 16384 {
            return Err("LSP stderr exceeds 16384 bytes; configure a quieter server".into());
        }
    }
}
