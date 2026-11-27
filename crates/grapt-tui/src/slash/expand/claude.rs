//! Command previews share argument substitution with skill activation.
pub(super) fn expand(
    template: &str,
    arguments: &str,
    paths: &grapt_client::wire::ClaudePluginPaths,
) -> Result<String, String> {
    grapt_plugin_prompt::expand(
        template,
        arguments,
        &grapt_plugin_prompt::Paths {
            root: &paths.root,
            data: &paths.data,
            project: &paths.project,
        },
    )
}
