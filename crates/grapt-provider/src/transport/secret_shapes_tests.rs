//! One case per secret shape, through the masking a failure message uses.
use crate::transport::error_text::mask;

fn masked(text: &str) -> String {
    mask(text.as_bytes(), "").as_str().to_owned()
}

fn hides(text: &str, secrets: &[&str]) {
    let held = masked(text);
    for secret in secrets {
        assert!(!held.contains(secret), "{secret} survived: {held}");
    }
}

fn keeps(text: &str, words: &[&str]) {
    let held = masked(text);
    for word in words {
        assert!(held.contains(word), "{word} was masked: {held}");
    }
}

#[test]
fn labelled_values_are_masked() {
    hides(
        r#"{"Authorization":"Basic dXNlcjpwYXNz","Cookie":"id=1","apiKey":"plain-value-a","access_token":"plain-value-b","client_secret":"plain-value-c","password":"plain-value-d"}"#,
        &["dXNlcjpwYXNz", "id=1", "plain-value"],
    );
}

#[test]
fn names_ending_in_key_are_masked() {
    hides(
        r#"{"secret_key":"value-one","secretKey":"value-two","private_key":"value-three","access_key":"value-four","key":"value-five","x-goog-api-key":"value-six"}"#,
        &["value-"],
    );
    keeps(r#"{"monkey":"banana","keys":3}"#, &["banana", "\"keys\":3"]);
}

#[test]
fn object_and_array_values_are_masked_whole() {
    hides(
        r#"{"token":{"value":"inner-one","scope":["inner-two"]},"authorization":["inner-three"],"after":"kept"}"#,
        &["inner-"],
    );
    keeps(
        r#"{"token":{"value":"x"},"after":"kept"}"#,
        &["\"after\":\"kept\""],
    );
}

#[test]
fn url_credentials_are_masked() {
    hides(
        "fetch https://deploy-user:deploy-pass@example.test/v1 failed",
        &["deploy-user", "deploy-pass"],
    );
    keeps(
        "fetch https://deploy-user:deploy-pass@example.test/v1 failed",
        &["https://", "@example.test/v1"],
    );
    // A comma or parentheses in a credential no longer ends the user-info.
    hides(
        "fetch https://user,name:(pass)@example.test/v1 failed",
        &["user,name", "(pass)"],
    );
}

#[test]
fn home_directories_are_masked_and_the_rest_of_the_path_kept() {
    hides(
        "cannot read /home/example/workspace/service.conf: permission denied",
        &["example"],
    );
    keeps(
        "cannot read /home/example/workspace/service.conf: permission denied",
        &["/home/", "/workspace/service.conf", "permission denied"],
    );
    hides("cannot read /Users/bob/project/service.conf", &["bob"]);
    hides(
        r#"cannot open C:\Users\example\project\service.conf"#,
        &["example"],
    );
    // A bare root names nobody, and prose that merely mentions users is kept.
    keeps("the users/ group is empty", &["users/", "group"]);
    keeps("path /home/ then a space", &["/home/"]);
}

#[test]
fn aws_keys_and_json_web_tokens_are_masked() {
    hides(
        "key AKIAIOSFODNN7EXAMPLE and token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
        &["IOSFODNN7EXAMPLE", "hbGciOiJIUzI1NiJ9", "c2lnbmF0dXJl"],
    );
}

#[test]
fn unlabelled_basic_credentials_are_masked_but_prose_is_kept() {
    hides("send Basic dXNlcjpwYXNz to retry", &["dXNlcjpwYXNz"]);
    keeps(
        "Basic authentication is required",
        &["Basic authentication"],
    );
}

#[test]
fn long_encoded_runs_are_masked() {
    hides(
        "signature 3f5a9c0e7b2d41f68a0c9e3b7d5f1a2c4e6b8d0f mismatch",
        &["3f5a9c0e7b2d41f68a0c9e3b7d5f1a2c"],
    );
    hides(
        "opaque dGhpcyBpcyBhIGxvbmcgYmFzZTY0IHZhbHVlIDEyMzQ1Ng== value",
        &["dGhpcyBpcyBhIGxvbmcgYmFzZTY0"],
    );
    keeps(
        "the request exceeded maximum_context_length_for_this_model",
        &["maximum_context_length_for_this_model"],
    );
}

#[test]
fn key_prefixes_need_a_token_body_and_a_word_start() {
    hides(
        "keys sk-ant-abcdef123456 gsk_abcdefgh1234 AIzaSyA1234567890abcdef ghp_abcdefgh12345678",
        &[
            "abcdef123456",
            "abcdefgh1234",
            "SyA1234567890",
            "abcdefgh12345678",
        ],
    );
    keeps(
        "ticket SK-1234 from Aizawa, task-abcdefghijk, sk-short",
        &["SK-1234", "Aizawa", "task-abcdefghijk", "sk-short"],
    );
}

#[test]
fn ordinary_fields_and_machine_codes_are_kept() {
    keeps(
        r#"{"error":{"type":"invalid_request_error","code":"invalid_api_key","message":"bad request for task-7"},"max_tokens":4096}"#,
        &[
            "invalid_request_error",
            "invalid_api_key",
            "bad request for task-7",
            "\"max_tokens\":4096",
        ],
    );
}

#[test]
fn a_json_escaped_windows_home_still_hides_its_account() {
    // Serializing first turns one Windows path into its escaped spelling, and the shape
    // finders read plain text, so masking serialized bytes missed the account.
    // The masker walks decoded strings, so both spellings are one case.
    for body in [
        r#"{"error":{"code":"invalid_request_error","message":"cannot read C:\\Users\\example\\project\\file"}}"#,
        r#"{"error":{"message":"cannot read /home/example/project/file"}}"#,
    ] {
        hides(body, &["example"]);
    }
    keeps(
        r#"{"error":{"message":"cannot read C:\\Users\\example\\project\\file"}}"#,
        &["cannot read", "project"],
    );
}

#[test]
fn every_url_query_and_fragment_is_masked_whatever_its_name() {
    // A query value has no closed alphabet, so the whole query and fragment are
    // masked rather than each guessed value. The `?` stays so the reader sees a
    // redaction rather than a deletion.
    hides(
        r#"{"error":{"message":"failed URL https://example.test/?session_id=private-session-marker"}}"#,
        &["private-session-marker", "session_id="],
    );
    hides(
        "failed GET https://example.test/path?a=one&b=two;c=three#frag",
        &["one", "two", "three", "frag"],
    );
    // Everything from the first `?` to whitespace goes, so a comma, a
    // parenthesis, an apostrophe, a semicolon and a `%2C` escape are all inside
    // the mask without being named anywhere.
    hides(
        "failed GET https://example.test/?a=1,2)3'4;5%2C6 tail",
        &["1,2)3'4;5%2C6"],
    );
    // The token's own prefix and the prose after the query are untouched.
    keeps(
        "failed GET https://example.test/path?a=one&b=two#frag here",
        &["https://example.test/path", "failed GET", "here"],
    );
}

/// One reviewer query-value control: the URL parser must keep the whole value,
/// and masking the body it arrives in must not let `private-query-tail` through.
fn review_query_value(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let value = reqwest::Url::parse(url)?
        .query_pairs()
        .next()
        .ok_or("query")?
        .1
        .into_owned();
    assert!(
        value.contains("private-query-tail"),
        "the URL parser must keep the whole value: {url} gave {value}"
    );
    hides(
        &format!(r#"{{"error":{{"message":"{url}"}}}}"#),
        &["private-query-tail"],
    );
    Ok(())
}

/// The query-value controls a reviewer raised across three rounds, one test per
/// character. Each is its own case so a failure names the character that broke
/// and the others still run: every one of these is legal inside a query value,
/// so the whole value must be masked without the masker naming any character.
#[test]
fn review_error_query_value_includes_a_literal_comma() -> Result<(), Box<dyn std::error::Error>> {
    review_query_value("https://example.test/?opaque=prefix,private-query-tail")
}

#[test]
fn review_error_query_value_includes_a_literal_parenthesis(
) -> Result<(), Box<dyn std::error::Error>> {
    review_query_value("https://example.test/?opaque=prefix)private-query-tail")
}

#[test]
fn review_error_query_value_includes_a_literal_semicolon() -> Result<(), Box<dyn std::error::Error>>
{
    review_query_value("https://example.test/?opaque=prefix;private-query-tail")
}

#[test]
fn review_error_query_value_includes_a_percent_escape() -> Result<(), Box<dyn std::error::Error>> {
    // The parser decodes `%2C` to a comma; the wire text still carries the
    // escape and must be masked whole.
    review_query_value("https://example.test/?opaque=prefix%2Cprivate-query-tail")
}

#[test]
fn review_error_query_value_includes_a_literal_apostrophe() -> Result<(), Box<dyn std::error::Error>>
{
    review_query_value("https://example.test/?opaque=prefix'private-query-tail")
}

#[test]
fn a_question_mark_in_prose_does_not_start_a_mask() {
    // Only a URL-looking token masks its tail: a sentence may ask a question
    // without losing the words after its `?`.
    for text in [
        "did the provider refuse? yes it did",
        "see yes/no? then retry",
        "question?answer",
    ] {
        keeps(text, &text.split_whitespace().collect::<Vec<_>>());
    }
    // A bare host and path still reads as a URL.
    hides(
        "GET example.test/path?opaque=private-tail",
        &["opaque=private-tail"],
    );
    // So does an absolute path.
    hides(
        "GET /api/v1/item?opaque=private-tail",
        &["opaque=private-tail"],
    );
}

#[test]
fn truncation_happens_after_masking_on_a_character_boundary() {
    // The bound is applied last, so a cut can never split a mask and leave a
    // fragment of the value it was hiding.
    let secret = "private-marker-abcdef";
    let body = format!(
        r#"{{"error":{{"message":"{} {} {secret}"}}}}"#,
        "é".repeat(200),
        "x".repeat(120)
    );
    let held = mask(body.as_bytes(), secret);
    let text = held.as_str();
    assert!(!text.contains(secret), "a truncated cut exposed the value");
    assert!(text.len() <= 512);
    assert!(std::str::from_utf8(text.as_bytes()).is_ok());
}
