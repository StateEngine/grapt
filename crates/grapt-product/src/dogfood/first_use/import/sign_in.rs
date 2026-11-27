//! Complete authentication-envelope retention.
//! Copyright 2025 OpenAI; licenses/openai-codex/.
//! Modified: use the existing bounded ChatGPT owner and new-profile staging,
//! and refuse account/access disagreement instead of importing mixed generations.
use super::*;
use grapt_login::chatgpt;

pub(super) fn capture(
    source: &Path,
    provider: &Spanned<ConfigValue>,
    inputs: &[(String, Vec<u8>)],
) -> Result<Option<chatgpt::Tokens>, String> {
    let field = |name| {
        provider
            .value()
            .get(name)
            .and_then(|value| value.value().as_str())
    };
    if field("id") != Some("openai")
        || field("credential_version") != Some(chatgpt::SIGN_IN_CREDENTIAL_VERSION)
        || field("credential").is_some_and(|value| value.starts_with("env:"))
    {
        return Ok(None);
    }
    let Some(tokens) = chatgpt::load(source)
        .map_err(|error| format!("account sign-in state cannot be imported: {error}"))?
    else {
        // Interactive first sign-in stages only an access credential here,
        // then completes its sign-in through the existing login owner.
        return Ok(None);
    };
    let credential = inputs
        .iter()
        .find(|(name, _)| name == chatgpt::CREDENTIAL_FILE)
        .map(|(_, bytes)| bytes.as_slice());
    if field("account") != Some(tokens.account.as_str())
        || credential != Some(tokens.access.as_bytes())
    {
        return Err("account sign-in and configured credential disagree; capture one coherent account generation before setup".into());
    }
    Ok(Some(tokens))
}
