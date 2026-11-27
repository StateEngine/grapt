//! Provider failures keep the existing connection and distinguish rejected keys.
use super::*;

#[derive(Clone, Copy)]
enum Provider {
    Google,
    Cohere,
    Cloudflare,
}
impl Provider {
    fn id(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Cohere => "cohere",
            Self::Cloudflare => "cloudflare-ai-gateway",
        }
    }
    fn reply(self, completion: bool, unfinished: bool) -> &'static str {
        match (self, completion, unfinished) {
            (Self::Google, false, _) => {
                r#"{"models":[{"name":"models/gemini-2.5-flash","inputTokenLimit":32768,"supportedGenerationMethods":["generateContent"]}]}"#
            }
            (Self::Cohere, false, _) => {
                r#"{"models":[{"name":"command-fixture","context_length":32768,"endpoints":["chat"]}]}"#
            }
            (Self::Google, true, false) => {
                r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"OK"}]}}]}"#
            }
            (Self::Google, true, true) => {
                r#"{"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[{"text":"Partial"}]}}]}"#
            }
            (Self::Cohere, true, false) => {
                r#"{"finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"OK"}]}}"#
            }
            (Self::Cohere, true, true) => {
                r#"{"finish_reason":"MAX_TOKENS","message":{"role":"assistant","content":[{"type":"text","text":"Partial"}]}}"#
            }
            (Self::Cloudflare, _, false) => {
                r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"OK"}]}]}"#
            }
            (Self::Cloudflare, _, true) => r#"{"status":"incomplete","output":[]}"#,
        }
    }
    fn completion(self, request: &str, body: &Value) -> Result<bool, String> {
        let (listing, completion, header) = match self {
            Self::Google => ("GET /models?", "POST /models/gemini-2.5-flash:generateContent HTTP/1.1", "x-goog-api-key: fixture-valid-"),
            Self::Cohere => ("GET /v1/models?", "POST /v2/chat HTTP/1.1", "authorization: bearer fixture-valid-"),
            Self::Cloudflare => ("", "POST /v1/0123456789abcdef0123456789abcdef/fixture-gateway/openai/responses HTTP/1.1", "cf-aig-authorization: bearer fixture-valid-"),
        };
        if !request.to_ascii_lowercase().contains(header) {
            return Err("provider credential header missing".into());
        }
        if !listing.is_empty() && request.starts_with(listing) {
            return Ok(false);
        }
        if !request.starts_with(completion) {
            return Err("unexpected provider verification route".into());
        }
        match self {
            Self::Google if !body["contents"].is_array() => Err("generation body missing".into()),
            Self::Cohere if body["model"] != "command-fixture" => Err("chat model changed".into()),
            Self::Cloudflare if !body["input"].is_array() => Err("gateway input missing".into()),
            _ => Ok(true),
        }
    }
}

fn failed_replacement(provider: Provider, rejected: bool) -> TestResult {
    let root = root(&format!("verification-{}-{rejected}", provider.id()));
    reset(&root);
    let bin = root.join("installed");
    fs::create_dir(&bin)?;
    let binary = bin.join(if cfg!(windows) { "grapt.exe" } else { "grapt" });
    fs::copy(shipped_cli::cli_binary(), &binary)?;
    fs::copy(
        shipped_cli::product_binary(),
        bin.join(if cfg!(windows) {
            "grapt-server.exe"
        } else {
            "grapt-server"
        }),
    )?;
    let expected = if matches!(provider, Provider::Cloudflare) {
        2
    } else {
        4
    };
    let KeyProviderFixture {
        endpoint,
        task,
        calls,
    } = key_provider_script(expected, 12000, move |request, body| {
        let completion = provider.completion(request, body)?;
        let probe = request.contains("fixture-valid-probe");
        if completion && probe && rejected {
            Ok(("403 Forbidden", "{}"))
        } else {
            Ok(("200 OK", provider.reply(completion, completion && probe)))
        }
    })?;
    let profile = root.join("profile");
    let mut args = vec![
        "login",
        provider.id(),
        "--id",
        "work",
        "--key-stdin",
        "--json",
    ];
    if matches!(provider, Provider::Cloudflare) {
        args.extend([
            "--cf-account",
            "0123456789abcdef0123456789abcdef",
            "--cf-gateway",
            "fixture-gateway",
            "--cf-provider",
            "openai",
        ]);
    }
    let (status, response, error) = cli_status(
        &binary,
        &profile,
        &endpoint,
        &args,
        Some(b"fixture-valid-seed\n"),
    )?;
    assert!(status.success(), "seed login refused: {response} {error}");
    let original = Store::read_snapshot(&profile)?.ok_or("seed connection absent")?;
    let credential = profile.join(
        &original
            .connections
            .first()
            .ok_or("seed credential absent")?
            .credential_file,
    );
    let material = fs::read(&credential)?;
    let (status, response, error) = cli_status(
        &binary,
        &profile,
        &endpoint,
        &args,
        Some(b"fixture-valid-probe\n"),
    )?;
    assert_eq!(
        status.code(),
        Some(if rejected { 13 } else { 12 }),
        "wrong failure class: {response} {error}"
    );
    assert_eq!(response["status"], "failed");
    if rejected {
        assert_eq!(response["reason"], "the provider rejected this key");
    } else {
        assert_ne!(response["reason"], "the provider rejected this key");
        assert!(response["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()));
    }
    assert_eq!(Store::read_snapshot(&profile)?, Some(original));
    assert!(
        fs::read(&credential)? == material,
        "failed replacement changed the stored credential"
    );
    assert_eq!(calls.load(Ordering::SeqCst), expected);
    task.join().map_err(|_| "provider fixture stopped")??;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn google_unfinished_completion_is_remote_failure() -> TestResult {
    failed_replacement(Provider::Google, false)
}
#[test]
fn google_rejected_key_has_credential_exit() -> TestResult {
    failed_replacement(Provider::Google, true)
}
#[test]
fn cohere_unfinished_completion_is_remote_failure() -> TestResult {
    failed_replacement(Provider::Cohere, false)
}
#[test]
fn cohere_rejected_key_has_credential_exit() -> TestResult {
    failed_replacement(Provider::Cohere, true)
}
#[test]
fn gateway_unfinished_completion_is_remote_failure() -> TestResult {
    failed_replacement(Provider::Cloudflare, false)
}
#[test]
fn gateway_rejected_key_has_credential_exit() -> TestResult {
    failed_replacement(Provider::Cloudflare, true)
}
