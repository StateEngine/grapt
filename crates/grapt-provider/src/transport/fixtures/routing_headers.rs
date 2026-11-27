//! Pin the actual ChatGPT wire headers independently of cache bucket sharing.
use super::*;
use crate::ProviderRequest;
use std::collections::BTreeMap;

#[tokio::test]
async fn cache05_chatgpt_headers_keep_cache_affinity_and_thread_identity_separate(
) -> Result<(), Box<dyn std::error::Error>> {
    let session: grapt_types::SessionId = "ses_actual_thread".parse()?;
    let own = crate::PromptCacheKey::for_session(&session)?;
    let shared = crate::PromptCacheKey::for_project("project", "openai", "gpt-5");
    for (cache, thread, sticky) in [
        (Some(own.clone()), true, false),
        (Some(shared.clone().with_session_header(&own)), true, false),
        (Some(shared), true, true),
        (None, true, false),
        (Some(own), false, false),
    ] {
        let fixture = serve(
            200,
            &[],
            vec![sse(&[r#"{"type":"response.completed","response":{}}"#])],
        )
        .await;
        let turn = if thread {
            crate::CodexTurn::for_session(session.clone())
        } else {
            crate::CodexTurn::default()
        };
        if sticky {
            let account = grapt_types::AccountId::new("acct_fixture").ok_or("account")?;
            turn.capture(&account, "sticky-token");
        }
        let request = ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
            model: model("openai", "gpt-5"),
            input: vec![user("Hello").into()],
            prompt_cache_key: cache.clone(),
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn,
        });
        let open = adapter(4)
            .open(input(
                "att_cache05_headers",
                &fixture.base,
                ProtocolFamily::ChatGptCodex,
                request,
            ))
            .await?;
        let _ = drain(open).await;
        let head = fixture.head.lock().map_err(|_| "fixture head")?.clone();
        let body = fixture.body.lock().map_err(|_| "fixture body")?.clone();
        let mut actual = BTreeMap::new();
        for line in head.lines().skip(1) {
            let (name, value) = line.split_once(':').ok_or("header framing")?;
            assert!(
                actual
                    .insert(name.to_owned(), value.trim().to_owned())
                    .is_none(),
                "duplicate request header"
            );
        }
        let mut expected = BTreeMap::from([
            (
                "authorization".into(),
                "bearer token-value-never-printed".into(),
            ),
            ("user-agent".into(), "secode-fixture/0.1".into()),
            ("accept".into(), "text/event-stream".into()),
            ("content-type".into(), "application/json".into()),
            ("originator".into(), "secode".into()), // grapt-rename: keep
            ("version".into(), "0.159.0".into()),
            ("chatgpt-account-id".into(), "acct_fixture".into()),
            (
                "host".into(),
                fixture.base.trim_start_matches("http://").into(),
            ),
            ("content-length".into(), body.len().to_string()),
        ]);
        if let Some(cache) = cache {
            expected.insert("session-id".into(), cache.session_header().into());
        }
        if thread {
            expected.insert("thread-id".into(), session.as_str().into());
            expected.insert("x-client-request-id".into(), session.as_str().into());
        }
        if sticky {
            expected.insert("x-codex-turn-state".into(), "sticky-token".into());
        }
        assert_eq!(actual, expected, "ChatGPT request header contract changed");
    }
    Ok(())
}
