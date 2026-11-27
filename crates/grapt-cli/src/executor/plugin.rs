use crate::command::plugin::{PluginAction, PluginCommand};
use crate::{CliLimits, ExitClass};
use grapt_api::u3a::{BoundedString, ClaudePluginRequestDto};

pub(super) async fn execute(command: PluginCommand, limits: CliLimits) -> u8 {
    let result = run(command, limits).await;
    let (class, message) = match result {
        Ok(message) => (ExitClass::Success, message),
        Err((class, message)) => (class, format!("grapt: {message}")),
    };
    if super::emit(message, class != ExitClass::Success, limits)
        .await
        .is_err()
    {
        return ExitClass::Output.code();
    }
    class.code()
}
async fn run(command: PluginCommand, limits: CliLimits) -> Result<String, (ExitClass, String)> {
    let usage = |reason: &str| (ExitClass::Usage, reason.to_owned());
    let directory = command
        .directory
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(|_| usage("plugin working directory is unavailable"))?;
    let directory = std::fs::canonicalize(directory)
        .map_err(|_| usage("plugin working directory cannot be resolved"))?;
    let request = match command.action {
        PluginAction::MarketplaceAdd(source) => ClaudePluginRequestDto::MarketplaceAdd {
            source: BoundedString::try_new(source)
                .map_err(|_| usage("plugin source exceeds 4096 bytes"))?,
        },
        PluginAction::Install(plugin) => ClaudePluginRequestDto::Install {
            plugin: BoundedString::try_new(plugin)
                .map_err(|_| usage("plugin identity exceeds 257 bytes"))?,
        },
    };
    let root = command
        .service
        .or_else(|| crate::command::terminal::user_profile(|name| std::env::var_os(name)))
        .ok_or_else(|| usage("plugin management requires a user profile"))?;
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .map_err(|_| usage("plugin profile path is unavailable"))?
            .join(root)
    };
    let client = super::connection_client::open(&root, true, limits)
        .await?
        .ok_or_else(|| usage("plugin profile is unavailable"))?;
    let directory = directory.to_string_lossy().into_owned();
    let result = tokio::select! {
        result=client.claude_plugin(&directory,&request)=>result.map(|response|response.message.as_str().escape_debug().to_string()).map_err(super::remote),
        _signal=super::termination_signal()=>Err((ExitClass::Cancelled,"plugin operation canceled; an identical command can be retried safely".into())),
    };
    let closed = super::connection_client::close(&client).await;
    result.and_then(|message| closed.map(|()| message))
}
