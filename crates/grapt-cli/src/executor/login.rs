//! `grapt login chatgpt`: sign in again with the device flow, fetch the
//! account's models, then let grapt-connections replace the profile's
//! sign-in in place. The profile is checked first, so a refusal costs no
//! browser round trip; nothing changes until the replacement commits.

use std::io::Write;
use std::path::{Path, PathBuf};

use grapt_connections::chatgpt::{self as connection, ResignError};
use grapt_types::ClockAuthority;

use crate::client::chatgpt::{self, SignInError, Tokens};
use crate::ExitClass;

type Failure = (ExitClass, String);

pub(super) async fn chatgpt(profile: Option<PathBuf>) -> u8 {
    match run(profile).await {
        Ok(()) => ExitClass::Success.code(),
        Err((class, message)) => {
            eprintln!("grapt: {message}");
            class.code()
        }
    }
}

async fn run(profile: Option<PathBuf>) -> Result<(), Failure> {
    let profile = match profile {
        Some(profile) => profile,
        None => crate::command::terminal::user_profile(|name| std::env::var_os(name)).ok_or((
            ExitClass::Usage,
            "no user configuration directory is known; name the profile with --profile <profile>"
                .to_owned(),
        ))?,
    };
    let configured = connection::check(&profile).map_err(refusal)?;
    let (client, issuer, backend) = route()?;
    let mut out = std::io::stderr();
    let _ = writeln!(
        out,
        "Signing in to ChatGPT again for the profile at {} (now account {}).",
        profile.display(),
        configured.account
    );
    let tokens = tokio::select! {
        tokens = sign_in(&client, &issuer, &mut out) => tokens.map_err(remote)?,
        _ = tokio::signal::ctrl_c() => {
            return Err((ExitClass::Cancelled, "sign-in cancelled; the profile was not changed".to_owned()))
        }
    };
    let listing = chatgpt::discover_models(&client, &backend, &tokens)
        .await
        .map_err(remote)?;
    let now = ClockAuthority::now(&chatgpt::SystemClock).as_millis();
    let resigned = connection::resign(&profile, &tokens, &listing, now)
        .await
        .map_err(refusal)?;
    println!("{}", outcome(&profile, &resigned));
    Ok(())
}

fn outcome(profile: &Path, resigned: &connection::Resigned) -> String {
    let mut text = format!(
        "Signed in to ChatGPT account {} for the profile at {}.",
        resigned.account,
        profile.display()
    );
    if resigned.account_changed() {
        text.push_str(&format!(
            "\nThe account changed from {}; the profile now names the new one. Restart any Grapt using this profile to switch. Its Sessions are kept.",
            resigned.previous_account
        ));
    } else {
        text.push_str(
            "\nA running Grapt using this profile uses the new sign-in on its next request.",
        );
    }
    text
}

async fn sign_in(
    client: &reqwest::Client,
    issuer: &str,
    out: &mut impl Write,
) -> Result<Tokens, SignInError> {
    let code = chatgpt::request_code(client, issuer).await?;
    let _ = writeln!(
        out,
        "Open {} in a browser and enter the code\n\n    {}\n\nWaiting for approval (Ctrl-C cancels)...",
        code.verification_url, code.user_code
    );
    chatgpt::wait_for_approval(client, issuer, &code, &chatgpt::SystemClock).await
}

/// The client, issuer and model backend. Production reaches ChatGPT over
/// HTTPS; the loopback testkit may name loopback fixtures for both.
fn route() -> Result<(reqwest::Client, String, String), Failure> {
    let client = |https_only| {
        crate::client::production_client(
            https_only,
            crate::client::Allowance::Request(crate::client::REQUEST_ALLOWANCE),
        )
        .map_err(|error| (ExitClass::Remote, error.to_string()))
    };
    #[cfg(feature = "loopback-testkit")]
    if let (Some(issuer), Some(backend)) = (
        std::env::var_os("GRAPT_TESTKIT_CHATGPT_ISSUER").and_then(|value| value.into_string().ok()),
        std::env::var_os("GRAPT_TESTKIT_CHATGPT_BACKEND")
            .and_then(|value| value.into_string().ok()),
    ) {
        return Ok((client(false)?, issuer, backend));
    }
    Ok((
        client(true)?,
        chatgpt::ISSUER.to_owned(),
        chatgpt::CODEX_BACKEND.to_owned(),
    ))
}

fn refusal(error: ResignError) -> Failure {
    match error {
        ResignError::Refused(reason) => (ExitClass::Usage, reason),
        // Storing can fail after the listing was replaced; a retry finishes it.
        ResignError::SignIn(error) => (
            ExitClass::Remote,
            format!("{error}; run grapt login chatgpt again to finish"),
        ),
    }
}

fn remote(error: SignInError) -> Failure {
    (
        ExitClass::Remote,
        format!("{error}; the profile was not changed"),
    )
}
