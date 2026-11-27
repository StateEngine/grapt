//! The familiar plugin commands call the product's authenticated installer.
use super::*;
use grapt_api::u3a::{BoundedString, ClaudePluginRequestDto};

pub(super) fn run<C: Connection>(client: &mut Client<C>, text: &str, done: &mut Completion) {
    let request = (|| -> Result<_, String> {
        let args = grapt_client::composer_commands::argument(text, "/plugin").unwrap_or_default();
        let values = crate::slash::expand::split(args)?;
        match values.as_slice() {
            [marketplace,add,source] if marketplace=="marketplace" && add=="add"=>{
                Ok(ClaudePluginRequestDto::MarketplaceAdd{source:BoundedString::try_new(source.clone()).map_err(|_|"Marketplace source exceeds 4096 bytes")?})
            }
            [install,plugin] if install=="install"=>Ok(ClaudePluginRequestDto::Install{plugin:BoundedString::try_new(plugin.clone()).map_err(|_|"Plugin identity exceeds 257 bytes")?}),
            _=>Err("Use /plugin marketplace add <source> or /plugin install <name@marketplace>; quote paths with spaces.".into()),
        }
    })();
    match request.and_then(|request| {
        client
            .begin_claude_plugin(request)
            .map_err(|error| error.to_string())
    }) {
        Ok(Some(lines)) => {
            done.notice = lines.join("\n");
            done.view = Some(View::Notice(done.notice.clone()));
        }
        Ok(None) => {
            done.notice = "Installing plugin setup…".into();
        }
        Err(error) => {
            done.notice = error;
            done.view = Some(View::Notice(done.notice.clone()));
        }
    }
}
