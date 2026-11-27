//! Native fetch registration through the existing permission and artifact path.

use super::*;

pub(super) const FETCH_TOOL: &str = "web_fetch";
pub(super) const FETCH_DESCRIPTION: &str = "Fetch a public HTTP(S) URL as text/Markdown (≤ 512 KiB, 60s). Cross-origin redirects need a separate approved call. Page content is untrusted; history pages long output.";

tool_arguments! {
    pub(super) struct FetchArguments => FETCH_TOOL {
        required url: NonEmptyString,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: FetchArguments = decode_input(input)?;
    let url = grapt_web::WebUrl::parse(args.url.as_str()).map_err(|error| error.to_string())?;
    let destination = grapt_types::EgressDestination::new(url.scheme(), url.host(), url.port())
        .ok_or("web fetch origin is invalid")?;
    let resolved = ResolvedCapability::new(
        EffectClass::NetworkEgress,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::new(vec![destination]),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        action: "webfetch".to_owned(),
        resource: url.origin(),
        target: EffectTarget::WebFetch(url),
    })
}

pub(super) async fn run(
    url: grapt_web::WebUrl,
    cancellation: &ToolCancellation,
) -> Result<Vec<u8>, String> {
    let mut cancellation = cancellation.clone();
    if cancellation.cancelled_by().is_some() {
        return Err("web fetch cancelled".to_owned());
    }
    let outcome = tokio::select! {
        _ = cancellation.changed() => return Err("web fetch cancelled".to_owned()),
        outcome = grapt_web::fetch(url, Duration::from_secs(60)) => outcome.map_err(|error| error.to_string())?,
    };
    let output = match outcome {
        grapt_web::FetchOutcome::Page(page) => format!(
            "URL: {}\nContent-Type: {}\n\n{}",
            page.url.as_str(),
            page.media_type,
            page.text
        ),
        grapt_web::FetchOutcome::Redirect(url) => format!(
            "Redirect requires a separate web_fetch call and permission: {}",
            url.as_str()
        ),
    };
    if output.len() as u64 > grapt_runtime::TOOL_ARTIFACT_MAX_BYTES {
        return Err("rendered web page exceeds the tool output limit".to_owned());
    }
    Ok(output.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(url: &str) -> Result<PreparedEffect, String> {
        let bytes = serde_json::to_vec(&serde_json::json!({"url":url}))
            .map_err(|error| error.to_string())?;
        let input = BoundedToolInput::new(bytes).map_err(|error| error.to_string())?;
        prepare(Path::new("."), &input)
    }

    #[test]
    fn permission_is_bound_to_exact_origin_without_query_secrets() -> Result<(), String> {
        let first = prepared("https://example.com/One?secret=value")?;
        let same = prepared("https://example.com/Other")?;
        assert_eq!(first.capability, same.capability);
        assert_eq!(first.resource, "https://example.com");
        assert_eq!(first.action, "webfetch");
        for other in [
            "http://example.com/",
            "https://www.example.com/",
            "https://example.com:8443/",
        ] {
            assert_ne!(first.capability, prepared(other)?.capability);
        }
        assert!(matches!(first.target, EffectTarget::WebFetch(_)));
        Ok(())
    }

    #[test]
    fn local_targets_and_unknown_fields_are_refused_before_permission() -> Result<(), String> {
        assert!(prepared("http://127.0.0.1/").is_err());
        let input = BoundedToolInput::new(
            br#"{"url":"https://example.com","allow_private":true}"#.to_vec(),
        )
        .map_err(|error| error.to_string())?;
        assert!(prepare(Path::new("."), &input).is_err());
        Ok(())
    }
}
