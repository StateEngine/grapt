use super::*;

/// Reproduce the backend's minimum-version filter on the actual query string.
pub(super) fn filter_listing(head: &str, answer: String) -> String {
    let Ok(mut listing) = serde_json::from_str::<Value>(&answer) else {
        return answer;
    };
    let Some(models) = listing.get_mut("models").and_then(Value::as_array_mut) else {
        return answer;
    };
    let target = head.split_whitespace().nth(1).unwrap_or_default();
    let url = reqwest::Url::parse(&format!("http://localhost{target}"))
        .unwrap_or_else(|error| panic!("fixture request URL: {error}"));
    let version = url
        .query_pairs()
        .find(|(key, _)| key == "client_version")
        .map(|(_, value)| version_parts(&value))
        .unwrap_or_default();
    models.retain(|model| {
        let Some(minimum) = model.get("minimal_client_version").and_then(Value::as_str) else {
            return true;
        };
        version >= version_parts(minimum)
    });
    listing.to_string()
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .unwrap_or_else(|error| panic!("fixture version: {error}"))
        })
        .collect()
}

#[tokio::test]
async fn discovery_survives_the_backend_minimum_client_version_filter() {
    let listing = serde_json::json!({"models": [{
        "slug": "gpt-5.5", "visibility": "list", "minimal_client_version": "0.153.0"
    }]})
    .to_string();
    let issuer = Issuer::start(vec![
        ("/models", 200, listing.clone()),
        ("/models", 200, listing.clone()),
    ])
    .await;
    let client = client();
    let old = client
        .get(format!("{}/models?client_version=0.1.0", issuer.base))
        .send()
        .await
        .unwrap_or_else(|error| panic!("old client request: {error}"))
        .bytes()
        .await
        .unwrap_or_else(|error| panic!("old client response: {error}"));
    let old: Value =
        serde_json::from_slice(&old).unwrap_or_else(|error| panic!("old client JSON: {error}"));
    assert_eq!(old, serde_json::json!({"models": []}));

    let bytes = discover_models(&client, &issuer.base, &signed_in(NOW + HOUR))
        .await
        .unwrap_or_else(|error| panic!("discovery: {error}"));
    assert_eq!(
        bytes,
        listing.as_bytes(),
        "a current client must see the account's models"
    );
    let seen = issuer.seen();
    let head = &seen[1].head;
    assert!(head.contains("/models?client_version=0.159.0 "), "{head}");
    assert!(head.contains("originator: secode\r\n"), "{head}"); // grapt-rename: keep
    assert!(head.contains("version: 0.159.0\r\n"), "{head}");
    assert!(
        head.contains(concat!(
            "user-agent: grapt/",
            env!("CARGO_PKG_VERSION"),
            "\r\n"
        )),
        "{head}"
    );
}
