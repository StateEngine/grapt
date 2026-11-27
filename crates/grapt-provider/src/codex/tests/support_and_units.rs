    use crate::normalize::StreamValidator;
    use crate::request::{InputMessage, MessageRole, ToolChoice, ToolDefinition};
    use grapt_policy::{PhysicalAttemptAuthoritySnapshot, ToolName, ToolVisibility};
    use grapt_types::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ModelRef, ModelRegistry, PolicyVersion, ProviderId, ResolvedModelTarget, RoutingRequest,
    };
    use grapt_types::{
        CapabilityId, EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement,
        ResolvedCapability,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn deadlines() -> CodexDeadlines {
        // Explicit small injected values. These are TEST values; no production
        // default exists and none is implied by their presence here.
        match CodexDeadlines::new(Duration::from_millis(500), Duration::from_secs(5)) {
            Ok(deadlines) => deadlines,
            Err(_) => panic!("both test deadlines are non-zero"),
        }
    }

    fn limits() -> CodexLimits {
        // An explicit FIXTURE capacity. It is not production policy: no
        // ratified concurrent-attempt count exists, so the production number
        // is a composition residual and this value says nothing about it.
        match std::num::NonZeroUsize::new(2) {
            Some(capacity) => CodexLimits::new(capacity),
            None => panic!("2 is non-zero"),
        }
    }

    fn model() -> ModelRef {
        match (
            ProviderId::new("openai"),
            ModelId::new("gpt-5.4"),
            BaseModelId::new("gpt-5.4"),
            ModelFamilyId::new("openai"),
            ModelCatalogVersion::new("test-catalog"),
        ) {
            (Some(provider), Some(model), Some(base), Some(family), Some(version)) => {
                let record =
                    ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
                let catalog = match ModelCatalog::load(version, vec![record]) {
                    Some(catalog) => catalog,
                    None => panic!("test catalog"),
                };
                match catalog.entry(&provider, &model, None) {
                    Some(entry) => ModelRef::from_catalog(entry),
                    None => panic!("catalog entry"),
                }
            }
            _ => panic!("test model coordinates must be non-empty"),
        }
    }

    fn request() -> ChatGptCodexRequest {
        ChatGptCodexRequest {
            model: model(),
            input: vec![InputMessage {
                role: MessageRole::User,
                content: "Hello".into(),
            }
            .into()],
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        }
    }

    fn lease() -> CredentialLease {
        match (AccountId::new("acct_test"), CredentialVersion::new("7")) {
            (Some(account), Some(version)) => {
                match CredentialLease::issue(
                    account,
                    version,
                    crate::credential::CredentialPlacement::BearerAuthorization,
                    Secret::new("token-value-never-printed".to_owned()),
                ) {
                    Ok(lease) => lease,
                    Err(_) => panic!("a non-empty test secret is a valid lease"),
                }
            }
            _ => panic!("test credential coordinates must be non-empty"),
        }
    }

    /// The route every test endpoint and target agrees on.
    fn route() -> ModelRouteId {
        match ModelRouteId::new("route_test") {
            Some(route) => route,
            None => panic!("test route id must be non-empty"),
        }
    }

    /// A registry-issued target, through the REAL path.
    ///
    /// This is what the nine `prepared_request` witnesses were blocked on: until
    /// `ModelRegistry` landed there was no route by which any component could
    /// hold a `ResolvedModelTarget`, so these fixtures could only carry inert
    /// `ModelRef` metadata where authority was required. Nothing here widens a
    /// seal — `ResolvedModelTarget::resolve` is still crate-private to
    /// `grapt-types`, and issuance is the narrower public door it always
    /// intended.
    fn issued_target() -> ResolvedModelTarget {
        let (Some(provider), Some(model), Some(base), Some(family), Some(version)) = (
            ProviderId::new("openai"),
            ModelId::new("gpt-5.4"),
            BaseModelId::new("gpt-5.4"),
            ModelFamilyId::new("openai"),
            ModelCatalogVersion::new("test-catalog"),
        ) else {
            panic!("test model coordinates must be non-empty")
        };
        let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
        let Some(catalog) = ModelCatalog::load(version, vec![record]) else {
            panic!("test catalog")
        };
        let registry = ModelRegistry::install(catalog);
        let (Some(account), Some(credential_version), Some(policy)) = (
            AccountId::new("acct_test"),
            CredentialVersion::new("7"),
            PolicyVersion::new("policy_test"),
        ) else {
            panic!("test routing coordinates must be non-empty")
        };
        match registry.issue(
            &provider,
            &model,
            None,
            RoutingRequest {
                route: route(),
                account,
                credential_version,
                policy,
            },
        ) {
            Ok(target) => target,
            Err(refusal) => panic!("the test catalog must issue its own row: {refusal:?}"),
        }
    }

    /// A bound carrier over the test coordinates. Every coordinate is derived
    /// from the SAME issuance, so a refusal here is a real defect rather than a
    /// fixture that disagreed with itself.
    /// An authority graph over a registry-issued target, with the named tools
    /// marked visible or not.
    fn authority(visible: &[(&str, bool)]) -> PhysicalAttemptAuthoritySnapshot {
        let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
        if builder.model(issued_target()).is_err() {
            panic!("a registry-issued target must enter its own authority graph");
        }
        for (name, is_visible) in visible {
            let Some(tool) = ToolName::new((*name).to_owned()) else {
                panic!("test tool name must be non-empty")
            };
            // Derived, never authored: `CapabilityId` has no public
            // constructor, so a tool's required capability is the canonical
            // form of a resolved ToolInvoke effect.
            let required = CapabilityId::derive(&ResolvedCapability::new(
                EffectClass::ToolInvoke,
                None,
                FilesystemAuthority::new(Vec::new(), Vec::new(), Vec::new()),
                EgressAuthority::default(),
                RealizedEnforcement::HarnessOnly,
            ));
            let visibility = ToolVisibility {
                tool,
                required,
                visible: *is_visible,
            };
            if builder.tool(visibility).is_err() {
                panic!("test tool visibility must be admissible");
            }
        }
        match builder.build() {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("test authority must build: {error:?}"),
        }
    }

    fn request_for(authority: &PhysicalAttemptAuthoritySnapshot) -> ChatGptCodexRequest {
        ChatGptCodexRequest {
            model: authority.model().model_ref().clone(),
            input: vec![InputMessage {
                role: MessageRole::User,
                content: "Hello".into(),
            }
            .into()],
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        }
    }

    fn carrier(
        id: &str,
        endpoint: ProviderEndpoint,
    ) -> ProviderAttemptCarrier<ProviderEndpoint, CredentialLease, ProviderRequest> {
        let authority = authority(&[]);
        let request = request_for(&authority);
        match ProviderAttemptCarrier::bind(
            attempt(id),
            authority,
            endpoint,
            lease(),
            ProviderRequest::ChatGptCodex(request),
        ) {
            Ok(carrier) => carrier,
            Err(refusal) => panic!("test coordinates must bind: {refusal:?}"),
        }
    }

    fn attempt(id: &str) -> PhysicalAttemptId {
        match PhysicalAttemptId::new(id) {
            Some(attempt) => attempt,
            None => panic!("test attempt id must be non-empty"),
        }
    }

    // -----------------------------------------------------------------------
    // Z01 carrier — the wire key set.
    // -----------------------------------------------------------------------

    #[test]
    fn codex_body_has_no_output_token_key() {
        // The type cannot carry `max_output_tokens` (compile-fail probe 04).
        // This is the other half of the law: the SERIALIZER must not invent the
        // key either, which no type can prevent.
        let body = codex_request_body(&request()).unwrap_or_else(|error| panic!("body: {error}"));
        let object = match body.as_object() {
            Some(object) => object,
            None => panic!("a Codex body is a JSON object"),
        };
        assert!(
            !object.contains_key("max_output_tokens"),
            "the Codex route must not send an output-token bound, not even as null"
        );
        assert!(!object.contains_key("max_tokens"));
        let rendered = body.to_string();
        assert!(
            !rendered.contains("max_output_tokens"),
            "no nested key may reintroduce the bound: {rendered}"
        );
        // Positive half: the keys that MUST be there are.
        assert_eq!(
            object.get("model").and_then(|v| v.as_str()),
            Some("gpt-5.4")
        );
        assert_eq!(object.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert!(object.contains_key("input"));
    }

    #[test]
    fn tools_and_tool_choice_are_named_explicitly() {
        let mut request = request();
        request.tools = vec![ToolDefinition {
            input_format: crate::ToolInputFormat::Json,
            name: "read".into(),
            description: None,
            input_schema: "{\"type\":\"object\"}".into(),
        }];
        request.tool_choice = Some(ToolChoice::Named {
            tool: "read".into(),
        });
        let body = codex_request_body(&request).unwrap_or_else(|error| panic!("body: {error}"));
        let rendered = body.to_string();
        assert!(rendered.contains("\"tools\""));
        assert!(rendered.contains("\"tool_choice\""));
        assert!(!rendered.contains("max_output_tokens"));
    }

    // -----------------------------------------------------------------------
    // Z02 carrier — production construction cannot select plaintext.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn production_client_refuses_plaintext() {
        // The adversarial witness required for this gate, with the control that
        // it attributable.
        //
        // An earlier version of this test sent the production client at a dead
        // port and asserted the request failed. That witness PASSED under the
        // mutation that deletes `https_only(true)` — a connection refusal is
        // also an error, so it could not tell "refused by scheme policy" from
        // "nothing was listening". The Z03 harness row caught it as a broken
        // witness, which is what the row is for.
        //
        // The fix is a same-URL control: one LIVE loopback listener, two
        // clients, opposite outcomes. If the production client's refusal came
        // from the network rather than from its scheme policy, the loopback
        // client would fail against the same URL too.
        let base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
        let url = format!("{base}/responses");

        let production = match build_client(deadlines()) {
            Ok(client) => client,
            Err(error) => panic!("production client must build: {error}"),
        };
        let refused = production.get(&url).send().await;
        assert!(
            refused.is_err(),
            "the production client must refuse a plaintext URL even when a server is listening on it"
        );

        // The control. The listener is still unconsumed, because the refusal
        // above never opened a socket — so a client that differs from the
        // production one by exactly the `https_only(true)` line reaches it.
        let loopback = match build_loopback_client(deadlines()) {
            Ok(client) => client,
            Err(error) => panic!("loopback client must build: {error}"),
        };
        let reached = loopback.get(&url).send().await;
        assert!(
            reached.is_ok(),
            "the control failed: this URL must be reachable, or the assertion above proves nothing"
        );

        // And the endpoint configuration refuses the same thing one layer
        // earlier — now through the one shared precedence every route uses.
        use crate::route::{EndpointConfig, EndpointSource, RouteConfigError};
        assert_eq!(
            EndpointConfig::new()
                .with_attempt_override("http://chatgpt.com/backend-api/codex")
                .resolve(crate::request::ProtocolFamily::ChatGptCodex)
                .err(),
            Some(RouteConfigError::EndpointNotHttps {
                level: EndpointSource::AttemptOverride
            })
        );
        assert!(EndpointConfig::new()
            .with_attempt_override("https://chatgpt.com/backend-api/codex")
            .resolve(crate::request::ProtocolFamily::ChatGptCodex)
            .is_ok());
    }

    #[test]
    fn zero_deadlines_are_refused() {
        assert_eq!(
            CodexDeadlines::new(Duration::ZERO, Duration::from_secs(1)),
            Err(CodexConfigError::ZeroDeadline { which: "connect" })
        );
        assert_eq!(
            CodexDeadlines::new(Duration::from_secs(1), Duration::ZERO),
            Err(CodexConfigError::ZeroDeadline {
                which: "response head"
            })
        );
    }

    #[test]
    fn an_empty_secret_is_not_a_lease() {
        let account = match AccountId::new("acct_test") {
            Some(account) => account,
            None => panic!("account"),
        };
        let version = match CredentialVersion::new("7") {
            Some(version) => version,
            None => panic!("version"),
        };
        assert_eq!(
            CredentialLease::issue(
                account,
                version,
                crate::credential::CredentialPlacement::BearerAuthorization,
                Secret::new(String::new())
            )
            .err(),
            Some(crate::credential::CredentialError::SecretEmpty)
        );
    }

    // -----------------------------------------------------------------------
    // Classification.
    // -----------------------------------------------------------------------

    #[test]
    fn status_classification_matches_the_ruled_policy() {
        assert_eq!(
            classify_status(401, None),
            ProviderFailureClass::Authentication
        );
        assert_eq!(
            classify_status(403, None),
            ProviderFailureClass::Authentication
        );
        assert!(!classify_status(401, None).is_retryable());
        assert_eq!(
            classify_status(429, Some(Duration::from_secs(3))),
            ProviderFailureClass::RateLimit {
                retry_after: Some(Duration::from_secs(3))
            }
        );
        assert!(classify_status(429, None).is_retryable());
        assert!(classify_status(503, None).is_retryable());
        assert_eq!(
            classify_status(400, None),
            ProviderFailureClass::InvalidRequest
        );
        assert_eq!(classify_status(404, None), ProviderFailureClass::NoRoute);
    }

    #[test]
    fn retry_after_parses_only_delta_seconds() {
        assert_eq!(parse_retry_after(Some("12")), Some(Duration::from_secs(12)));
        assert_eq!(
            parse_retry_after(Some(" 12 ")),
            Some(Duration::from_secs(12))
        );
        // An HTTP-date is not parsed, and absence is truthful rather than
        // fabricated as zero.
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn finish_reason_transcribes_the_oracle_mapping() {
        let incomplete = serde_json::json!({
            "type": "response.incomplete",
            "response": { "incomplete_details": { "reason": "max_output_tokens" } }
        });
        assert_eq!(
            finish_reason_for("response.incomplete", &incomplete),
            FinishReason::Length
        );
        let filtered = serde_json::json!({
            "type": "response.incomplete",
            "response": { "incomplete_details": { "reason": "content_filter" } }
        });
        assert_eq!(
            finish_reason_for("response.incomplete", &filtered),
            FinishReason::ContentFilter
        );
        let bare = serde_json::json!({ "type": "response.incomplete", "response": {} });
        assert_eq!(
            finish_reason_for("response.incomplete", &bare),
            FinishReason::Unknown
        );
        let completed = serde_json::json!({ "type": "response.completed", "response": {} });
        assert_eq!(
            finish_reason_for("response.completed", &completed),
            FinishReason::Stop
        );
    }

    #[test]
    fn length_finish_is_truncated_never_idle() {
        // Contract rule 11, proven through the route this adapter actually
        // takes: a Codex `incomplete_details.reason = max_output_tokens`
        // becomes `FinishReason::Length`, and `Length` normalizes to
        // `Truncated` — never an ordinary stop and never idle.
        let incomplete = serde_json::json!({
            "type": "response.incomplete",
            "response": { "incomplete_details": { "reason": "max_output_tokens" } }
        });
        let finish = finish_reason_for("response.incomplete", &incomplete);
        assert_eq!(finish, FinishReason::Length);
        assert_eq!(
            finish.normalize(),
            crate::fragment::NormalizedFinish::Completed {
                completion: grapt_types::ProviderCompletionDisposition::Truncated,
            }
        );
        assert_ne!(
            finish.normalize(),
            crate::fragment::NormalizedFinish::Completed {
                completion: grapt_types::ProviderCompletionDisposition::AssertedComplete,
            }
        );
    }

    #[test]
    fn the_sse_decoder_is_bounded_before_it_grows() {
        let mut decoder = SseDecoder::new();
        let oversized = vec![b'x'; AdapterState::MAX_BYTES + 1];
        assert!(
            decoder.feed(&oversized).is_err(),
            "a frame larger than the capture bound must be refused, not buffered"
        );
        // And it stays refused: an overflowed decoder does not recover.
        assert!(decoder.feed(b"data: {}\n\n").is_err());
    }

    #[test]
    fn the_sse_decoder_reassembles_split_frames() {
        let mut decoder = SseDecoder::new();
        let first = match decoder.feed(b"data: {\"type\":\"a\"}") {
            Ok(payloads) => payloads,
            Err(error) => panic!("a partial frame is not an error: {error:?}"),
        };
        assert!(first.is_empty(), "an incomplete frame yields nothing yet");
        let second = match decoder.feed(b"\n\ndata: {\"type\":\"b\"}\n\n") {
            Ok(payloads) => payloads,
            Err(error) => panic!("two complete frames: {error:?}"),
        };
        assert_eq!(second.len(), 2);
    }
