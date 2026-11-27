use super::*;

const ISSUER: &str = "https://issuer.example";

fn metadata(issuer_callback: bool) -> Result<OAuthMetadata, OAuthError> {
    let wire = serde_json::json!({
        "issuer":ISSUER, "authorization_endpoint":"https://issuer.example/authorize",
        "token_endpoint":"https://issuer.example/token", "code_challenge_methods_supported":["S256"],
        "authorization_response_iss_parameter_supported":issuer_callback,
    });
    OAuthMetadata::parse(wire.to_string().as_bytes(), ISSUER)
}

fn flow(issuer_callback: bool) -> Result<AuthorizationFlow, OAuthError> {
    AuthorizationFlow::new(
        metadata(issuer_callback)?,
        "https://mcp.example/endpoint?tenant=one",
        "client-id",
        18453,
        Secret::new([1; 32]),
        Secret::new([2; 32]),
        Some("read write"),
    )
}

fn callback(flow: &AuthorizationFlow, issuer: Option<&str>, result: (&str, &str)) -> String {
    let mut url = flow.redirect_uri().clone();
    let mut query = url.query_pairs_mut();
    query.append_pair("state", flow.state.expose());
    if let Some(issuer) = issuer {
        query.append_pair("iss", issuer);
    }
    query.append_pair(result.0, result.1);
    drop(query);
    url.to_string()
}

#[test]
fn authorization_is_pkce_bound_and_a_callback_is_consumed_once() -> Result<(), OAuthError> {
    let mut flow = flow(true)?;
    let authorization =
        Url::parse(flow.authorization_url().expose()).map_err(|_| OAuthError::Format)?;
    let parameters: std::collections::BTreeMap<_, _> = authorization.query_pairs().collect();
    assert_eq!(
        parameters.get("code_challenge").map(|value| value.as_ref()),
        Some("VtX6czP210fbQsI5QH5dpMMvTHnzXQkrE0_TWkAtnFw")
    );
    assert_eq!(
        parameters
            .get("code_challenge_method")
            .map(|value| value.as_ref()),
        Some("S256")
    );
    assert_eq!(
        parameters.get("resource").map(|value| value.as_ref()),
        Some("https://mcp.example/endpoint?tenant=one")
    );
    assert_eq!(
        parameters.get("scope").map(|value| value.as_ref()),
        Some("read write")
    );
    let callback = callback(&flow, Some(ISSUER), ("code", "code+with&escapes"));
    let exchange = flow.accept_callback(&callback)?;
    let fields: std::collections::BTreeMap<_, _> =
        url::form_urlencoded::parse(exchange.form.expose().as_bytes()).collect();
    assert_eq!(
        fields.get("code").map(|value| value.as_ref()),
        Some("code+with&escapes")
    );
    assert_eq!(
        fields.get("resource").map(|value| value.as_ref()),
        Some("https://mcp.example/endpoint?tenant=one")
    );
    assert_eq!(
        fields.get("code_verifier").map(|value| value.len()),
        Some(43)
    );
    assert_eq!(
        flow.accept_callback(&callback).err(),
        Some(OAuthError::Consumed)
    );
    assert!(!format!("{flow:?}").contains("client-id"));
    assert!(!format!("{exchange:?}").contains("code+with"));
    Ok(())
}

#[test]
fn issuer_is_compared_exactly_before_success_or_error_handling() -> Result<(), OAuthError> {
    let mut flow = flow(true)?;
    for issuer in [
        None,
        Some("https://issuer.example/"),
        Some("https://ISSUER.example"),
        Some("https://other.example"),
    ] {
        for result in [("code", "code"), ("error", "untrusted denial text")] {
            let callback = callback(&flow, issuer, result);
            assert_eq!(
                flow.accept_callback(&callback).err(),
                Some(OAuthError::Issuer)
            );
        }
    }
    let denied = callback(&flow, Some(ISSUER), ("error", "denied"));
    assert_eq!(
        flow.accept_callback(&denied).err(),
        Some(OAuthError::Denied)
    );
    assert_eq!(
        flow.accept_callback(&denied).err(),
        Some(OAuthError::Consumed)
    );
    let mut legacy = self::flow(false)?;
    let wrong = callback(&legacy, Some("https://other.example"), ("code", "code"));
    assert_eq!(
        legacy.accept_callback(&wrong).err(),
        Some(OAuthError::Issuer)
    );
    let accepted = callback(&legacy, None, ("code", "code"));
    assert!(legacy.accept_callback(&accepted).is_ok());
    Ok(())
}

#[test]
fn bad_state_duplicate_parameters_and_cross_server_paths_do_not_consume_login(
) -> Result<(), OAuthError> {
    let mut flow = flow(true)?;
    let good = callback(&flow, Some(ISSUER), ("code", "code"));
    let mut wrong_path = Url::parse(&good).map_err(|_| OAuthError::Format)?;
    wrong_path.set_path("/mcp/oauth/other-server");
    let wrong_state = good.replace(flow.state.expose(), "not-the-state");
    for bad in [
        wrong_path.to_string(),
        wrong_state,
        format!("{good}&code=again"),
        format!("{good}&iss=https%3A%2F%2Fissuer.example"),
        format!("{good}&error=denied"),
        format!("{good}&extra=%ff"),
        format!("{good}&extra=%xy"),
    ] {
        assert!(flow.accept_callback(&bad).is_err());
    }
    assert!(flow.accept_callback(&good).is_ok());
    Ok(())
}

#[test]
fn metadata_refuses_mismatched_issuer_unsafe_endpoints_and_absent_pkce() -> Result<(), OAuthError> {
    let original = serde_json::json!({"issuer":ISSUER, "authorization_endpoint":"https://issuer.example/authorize",
        "token_endpoint":"https://issuer.example/token", "code_challenge_methods_supported":["S256"]});
    assert_eq!(
        OAuthMetadata::parse(original.to_string().as_bytes(), "https://issuer.example/").err(),
        Some(OAuthError::Issuer)
    );
    for endpoint in [
        "http://issuer.example/token",
        "file:///tmp/token",
        "https://u:p@issuer.example/token",
        "https://issuer.example/token#fragment",
        "https://issuer.example/token?code=preloaded",
    ] {
        let mut wire = original.clone();
        wire["token_endpoint"] = endpoint.into();
        assert!(OAuthMetadata::parse(wire.to_string().as_bytes(), ISSUER).is_err());
    }
    let mut unsupported = original.clone();
    unsupported["code_challenge_methods_supported"] = serde_json::json!(["plain"]);
    assert_eq!(
        OAuthMetadata::parse(unsupported.to_string().as_bytes(), ISSUER).err(),
        Some(OAuthError::Pkce)
    );
    assert!(OAuthMetadata::parse(&vec![b' '; 65537], ISSUER).is_err());
    assert!(AuthorizationFlow::new(
        metadata(true)?,
        "http://mcp.example/",
        "client",
        18453,
        Secret::new([1; 32]),
        Secret::new([2; 32]),
        None
    )
    .is_err());
    Ok(())
}

#[test]
fn metadata_refuses_duplicate_fields_and_url_whitespace() {
    let original = serde_json::json!({"issuer":ISSUER,"authorization_endpoint":"https://issuer.example/authorize","token_endpoint":"https://issuer.example/token","code_challenge_methods_supported":["S256"]});
    let duplicate = original
        .to_string()
        .replacen('{', &format!("{{\"issuer\":\"{ISSUER}\","), 1);
    assert!(OAuthMetadata::parse(duplicate.as_bytes(), ISSUER).is_err());
    for field in ["issuer", "authorization_endpoint", "token_endpoint"] {
        let mut value = original.clone();
        value[field] = format!(" {}", original[field].as_str().unwrap_or_default()).into();
        assert!(OAuthMetadata::parse(value.to_string().as_bytes(), ISSUER).is_err());
    }
}

#[test]
fn callback_path_is_exact_before_url_normalization() -> Result<(), OAuthError> {
    let mut flow = flow(true)?;
    let good = callback(&flow, Some(ISSUER), ("code", "held-code"));
    let normalized = good.replacen("/mcp/oauth/", "/elsewhere/../mcp/oauth/", 1);
    assert_eq!(
        flow.accept_callback(&normalized).err(),
        Some(OAuthError::Callback)
    );
    assert!(flow.accept_callback(&good).is_ok());
    Ok(())
}
