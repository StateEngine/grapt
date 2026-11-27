use grapt_web::{FetchError, WebUrl};

#[test]
fn public_url_preserves_case_query_and_normalizes_origin() -> Result<(), FetchError> {
    let url = WebUrl::parse("HTTPS://EXAMPLE.COM:443/Case/Sensitive?token=secret#anchor")?;
    assert_eq!(
        url.as_str(),
        "https://example.com/Case/Sensitive?token=secret"
    );
    assert_eq!(url.origin(), "https://example.com");
    assert!(!format!("{url:?}").contains("secret"));
    assert!(!format!("{url:?}").contains("Sensitive"));
    Ok(())
}

#[test]
fn local_reserved_ambiguous_and_credential_urls_are_refused() {
    for url in [
        "file:///etc/passwd",
        "https://user:secret@example.com/",
        "http://localhost/",
        "http://printer/",
        "http://printer.local/",
        "http://node.home.arpa/",
        "http://localhost./",
        "http://127.1/",
        "http://2130706433/",
        "http://0x7f000001/",
        "http://10.0.0.1/",
        "http://100.64.0.1/",
        "http://169.254.169.254/",
        "http://172.31.255.255/",
        "http://192.0.2.1/",
        "http://192.88.99.1/",
        "http://192.168.1.1/",
        "http://198.18.0.1/",
        "http://198.51.100.2/",
        "http://203.0.113.1/",
        "http://224.0.0.1/",
        "http://240.0.0.1/",
        "http://[::1]/",
        "http://[::ffff:8.8.8.8]/",
        "http://[64:ff9b::7f00:1]/",
        "http://[2001:db8::1]/",
        "http://[2002:7f00:1::]/",
        "http://[fc00::1]/",
        "http://[fe80::1]/",
        "http://[3fff::1]/",
        "https://example.com/\npath",
    ] {
        assert!(WebUrl::parse(url).is_err(), "accepted {url}");
    }
    assert!(WebUrl::parse(&format!("https://example.com/{}", "x".repeat(8192))).is_err());
}

#[test]
fn ordinary_public_v4_and_v6_are_accepted() {
    for url in [
        "https://8.8.8.8/",
        "https://[2606:4700:4700::1111]/",
        "http://93.184.215.14/",
    ] {
        assert!(WebUrl::parse(url).is_ok(), "refused {url}");
    }
}
