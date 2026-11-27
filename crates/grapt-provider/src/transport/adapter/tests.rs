use super::{routing_session, session_routed, SESSION_ROUTED_HOST};
use crate::request::{PromptCacheKey, ProtocolFamily, ProviderRequest};
use crate::transport::fixtures::{compat_request, input};

#[test]
fn only_the_gateway_and_its_subdomains_receive_the_session_header() {
    let host = SESSION_ROUTED_HOST;
    assert!(session_routed(&format!(
        "https://{host}/inference/go/openai/v1"
    )));
    assert!(session_routed(&format!("https://api.{host}/v1")));
    assert!(!session_routed(&format!("https://{host}.example.com/v1")));
    assert!(!session_routed(&format!("https://not{host}/v1")));
    assert!(!session_routed("https://api.x.ai/v1"));
    assert!(!session_routed("not a url"));
}

#[test]
fn another_endpoint_never_learns_the_session() -> Result<(), Box<dyn std::error::Error>> {
    let mut request = compat_request();
    let ProviderRequest::OpenAiCompatible(compat) = &mut request else {
        return Err("fixture family".into());
    };
    compat.routing_session = Some(PromptCacheKey::try_new("ses-a".to_owned())?);
    let attempt = input(
        "pat_session",
        "http://127.0.0.1:1",
        ProtocolFamily::OpenAiCompatible,
        request,
    );
    assert_eq!(routing_session(&attempt), None);
    Ok(())
}
