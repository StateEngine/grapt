use super::*;
use crate::transport::adapter::AttemptInput;
use crate::transport::fixtures::{adapter, anthropic_request, input};
use crate::{CodexInputItem, ProtocolFamily, ProviderRequest};

fn bound_input() -> Result<AttemptInput, Box<dyn std::error::Error>> {
    binding_input(false)
}

fn binding_input(effort: bool) -> Result<AttemptInput, Box<dyn std::error::Error>> {
    let mut input = input(
        "pc-binding",
        "http://127.0.0.1:1",
        ProtocolFamily::AnthropicMessages,
        anthropic_request(),
    );
    let binding = ReplayBinding::from_input(&input)?;
    let state = thinking_state(
        "inspect".into(),
        "PC_SIGNED_SENTINEL".into(),
        &binding,
        crate::AnthropicInstructionMode::CurrentPrefix,
    )?;
    let model = input.request.model();
    let mut replay = ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(state.capture_bytes().ok_or("state")?)?,
    )?;
    let ProviderRequest::AnthropicMessages(request) = &mut input.request else {
        return Err("family".into());
    };
    if effort {
        request.effort = crate::EffortPolicy {
            mode: crate::EffortMode::Chronological,
            default: Some(crate::ReasoningEffort::Medium),
        };
        replay.variant = grapt_types::ModelVariantId::new("low");
    }
    request
        .messages
        .push(CodexInputItem::ProviderReplay(replay));
    Ok(input)
}

#[test]
fn pc_replay_binding_preserves_current_target_and_drops_after_endpoint_route_account_version_changes(
) -> Result<(), Box<dyn std::error::Error>> {
    for effort in [false, true] {
        assert!(adapter(1).stage(binding_input(effort)?).is_ok());
        for changed in ["endpoint", "route", "account", "credential-version"] {
            let mut input = binding_input(effort)?;
            match changed {
                "endpoint" | "route" => {
                    let base = crate::route::LoopbackBase::parse(if changed == "endpoint" {
                        "http://127.0.0.1:2"
                    } else {
                        "http://127.0.0.1:1"
                    })?;
                    let route = if changed == "route" {
                        grapt_types::ModelRouteId::new("other-route").ok_or("route")?
                    } else {
                        input.endpoint.route().clone()
                    };
                    input.endpoint = crate::ProviderEndpoint::loopback(
                        &base,
                        ProtocolFamily::AnthropicMessages,
                        route,
                    );
                }
                _ => {
                    input.lease = crate::CredentialLease::issue(
                        if changed == "account" {
                            grapt_types::AccountId::new("other-account").ok_or("account")?
                        } else {
                            input.lease.account().clone()
                        },
                        if changed == "credential-version" {
                            grapt_types::CredentialVersion::new("other-version").ok_or("version")?
                        } else {
                            input.lease.credential_version().clone()
                        },
                        crate::CredentialPlacement::AnthropicApiKey,
                        grapt_types::Secret::new("different-secret-never-hashed".to_owned()),
                    )?;
                }
            }
            drop_unreplayable_reasoning(&mut input);
            assert_eq!(replays(&input), 0, "{changed} must drop the thinking");
            assert!(
                adapter(1).stage(input).is_ok(),
                "{changed} refused the turn"
            );
        }
    }
    Ok(())
}

#[test]
fn pc_replay_framing_version_corruption_bounds_and_debug_are_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    assert_ne!(
        ReplayBinding::from_fields(["ab", "c", "d", "e"])?,
        ReplayBinding::from_fields(["a", "bc", "d", "e"])?
    );
    assert!(ReplayBinding::from_fields([&"x".repeat(16385), "", "", ""]).is_err());
    let input = bound_input()?;
    let ProviderRequest::AnthropicMessages(request) = &input.request else {
        return Err("family".into());
    };
    let binding = ReplayBinding::from_input(&input)?;
    let state = thinking_state(
        "inspect".into(),
        "PC_SIGNED_SENTINEL".into(),
        &binding,
        crate::AnthropicInstructionMode::CurrentPrefix,
    )?;
    assert!(!format!("{state:?} {binding:?} {request:?}").contains("PC_SIGNED_SENTINEL"));
    let raw = std::str::from_utf8(state.capture_bytes().ok_or("state")?)?;
    for bad in [
        raw.replace("anthropic-thinking-v1", "anthropic-thinking-v3"),
        raw.replace("signature", "unknown"),
        raw.replace("PC_SIGNED_SENTINEL", ""),
        "x".repeat(AdapterState::MAX_BYTES + 1),
    ] {
        assert!(ProviderReplay::restore(
            request.model.provider().clone(),
            request.model.model().clone(),
            None,
            &bad
        )
        .is_err());
    }
    assert!(thinking_state(
        "x".repeat(AdapterState::MAX_BYTES),
        "signature".into(),
        &binding,
        crate::AnthropicInstructionMode::CurrentPrefix,
    )
    .is_err());
    Ok(())
}

#[test]
fn native_epoch_provenance_preserves_vendor_blocks_and_reads_legacy_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let input = bound_input()?;
    let binding = ReplayBinding::from_input(&input)?;
    let model = input.request.model();
    for redacted in [false, true] {
        let mut vendor = Vec::new();
        for mode in [
            crate::AnthropicInstructionMode::CurrentPrefix,
            crate::AnthropicInstructionMode::Epochs,
        ] {
            let state = if redacted {
                redacted_state("PC_REDACTED_SENTINEL".into(), &binding, mode)?
            } else {
                thinking_state(
                    "inspect".into(),
                    "PC_SIGNED_SENTINEL".into(),
                    &binding,
                    mode,
                )?
            };
            let replay = ProviderReplay::restore(
                model.provider().clone(),
                model.model().clone(),
                model.variant().cloned(),
                std::str::from_utf8(state.capture_bytes().ok_or("state")?)?,
            )?;
            assert_eq!(
                replay.requires_legacy_anthropic_prefix(),
                mode == crate::AnthropicInstructionMode::CurrentPrefix
            );
            assert!(replay.bound_to(&binding));
            assert!(!format!("{replay:?}").contains("SENTINEL"));
            vendor.push(replay.anthropic_block().ok_or("native block")?);
        }
        assert_eq!(
            vendor[0], vendor[1],
            "provenance never changes vendor replay bytes"
        );
    }
    Ok(())
}

#[test]
fn pc_replay_does_not_cross_model_or_protocol_even_with_the_same_binding(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut input = bound_input()?;
    let other = crate::transport::fixtures::responses_request();
    let ProviderRequest::AnthropicMessages(request) = &mut input.request else {
        return Err("family".into());
    };
    request.model = other.model().clone();
    drop_unreplayable_reasoning(&mut input);
    assert_eq!(replays(&input), 0, "another model must drop the thinking");
    let input = bound_input()?;
    let ProviderRequest::AnthropicMessages(request) = input.request else {
        return Err("family".into());
    };
    let wrong_protocol = ProviderRequest::OpenAiResponses(crate::OpenAiResponsesRequest {
        model: request.model,
        input: request.messages,
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: request.tools,
        web_search: None,
        tool_choice: request.tool_choice,
    });
    assert!(crate::routes::request_validation::validate(&wrong_protocol).is_err());
    Ok(())
}

fn reasoning_item(encrypted: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "reasoning",
        "id": "rs_pc_1",
        "status": "completed",
        "summary": [{"type": "summary_text", "text": "inspect the file"}],
        "encrypted_content": encrypted,
    })
}

/// A Responses input that carries one reasoning replay produced under its own
/// binding and model.
fn reasoning_input() -> Result<AttemptInput, Box<dyn std::error::Error>> {
    let mut input = input(
        "pc-reasoning",
        "http://127.0.0.1:1",
        ProtocolFamily::OpenAiResponses,
        crate::transport::fixtures::responses_request(),
    );
    let binding = ReplayBinding::for_reasoning(&input).ok_or("binding")?;
    let state = reasoning_state(&reasoning_item("PC_ENCRYPTED_SENTINEL"), &binding)
        .ok_or("reasoning state")?;
    let model = input.request.model();
    let replay = ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(state.capture_bytes().ok_or("state")?)?,
    )?;
    let ProviderRequest::OpenAiResponses(request) = &mut input.request else {
        return Err("family".into());
    };
    request.input.push(CodexInputItem::ProviderReplay(replay));
    Ok(input)
}

fn replays(input: &AttemptInput) -> usize {
    let items = match &input.request {
        ProviderRequest::OpenAiResponses(request) => &request.input,
        ProviderRequest::ChatGptCodex(request) => &request.input,
        ProviderRequest::AnthropicMessages(request) | ProviderRequest::VertexAnthropic(request) => {
            &request.messages
        }
        ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
            &request.messages
        }
        ProviderRequest::BedrockConverse(request) => &request.messages,
        ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
            &request.messages
        }
        ProviderRequest::CohereChat(request) => &request.messages,
    };
    items
        .iter()
        .filter(|item| matches!(item, CodexInputItem::ProviderReplay(_)))
        .count()
}

/// `reasoning_input` after one switch away from the item's own binding.
fn switched(changed: &str) -> Result<AttemptInput, Box<dyn std::error::Error>> {
    switched_from(reasoning_input()?, changed)
}

fn switched_from(
    mut input: AttemptInput,
    changed: &str,
) -> Result<AttemptInput, Box<dyn std::error::Error>> {
    match changed {
        "model" => {
            let ProviderRequest::OpenAiResponses(request) = &mut input.request else {
                return Err("family".into());
            };
            request.model = crate::transport::fixtures::compat_request().model().clone();
        }
        "endpoint" => {
            input.endpoint = crate::ProviderEndpoint::loopback(
                &crate::route::LoopbackBase::parse("http://127.0.0.1:2")?,
                ProtocolFamily::OpenAiResponses,
                input.endpoint.route().clone(),
            );
        }
        "codex" => {
            // The same endpoint, lease and model under the other Responses family.
            let ProviderRequest::OpenAiResponses(request) = &input.request else {
                return Err("family".into());
            };
            let codex = ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
                model: request.model.clone(),
                input: request.input.clone(),
                prompt_cache_key: None,
                tools: Vec::new(),
                web_search: None,
                tool_choice: None,
                reasoning: Default::default(),
                turn: Default::default(),
            });
            input.endpoint = crate::ProviderEndpoint::loopback(
                &crate::route::LoopbackBase::parse("http://127.0.0.1:1")?,
                ProtocolFamily::ChatGptCodex,
                input.endpoint.route().clone(),
            );
            input.request = codex;
        }
        "account" => {
            input.lease = crate::CredentialLease::issue(
                grapt_types::AccountId::new("other-account").ok_or("account")?,
                input.lease.credential_version().clone(),
                crate::CredentialPlacement::BearerAuthorization,
                grapt_types::Secret::new("different-secret-never-hashed".to_owned()),
            )?;
        }
        _ => {
            // A switch to another protocol keeps the history, reasoning included.
            let ProviderRequest::OpenAiResponses(request) = &input.request else {
                return Err("family".into());
            };
            let messages = request.input.clone();
            if changed == "anthropic" {
                let mut other = anthropic_request();
                let ProviderRequest::AnthropicMessages(target) = &mut other else {
                    return Err("family".into());
                };
                target.messages = messages;
                input = crate::transport::fixtures::input(
                    "pc-reasoning",
                    "http://127.0.0.1:1",
                    ProtocolFamily::AnthropicMessages,
                    other,
                );
            } else {
                let mut other = crate::transport::fixtures::compat_request();
                let ProviderRequest::OpenAiCompatible(target) = &mut other else {
                    return Err("family".into());
                };
                target.messages = messages;
                input = crate::transport::fixtures::input(
                    "pc-reasoning",
                    "http://127.0.0.1:1",
                    ProtocolFamily::OpenAiCompatible,
                    other,
                );
            }
        }
    }
    Ok(input)
}

#[test]
fn pc_reasoning_replay_is_kept_on_its_binding_and_dropped_after_any_switch(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut kept = reasoning_input()?;
    drop_unreplayable_reasoning(&mut kept);
    assert_eq!(
        replays(&kept),
        1,
        "the same binding and model keep the item"
    );
    assert!(adapter(1).stage(reasoning_input()?).is_ok());
    for changed in [
        "model",
        "endpoint",
        "codex",
        "account",
        "anthropic",
        "compatible",
    ] {
        let mut dropped = switched(changed)?;
        drop_unreplayable_reasoning(&mut dropped);
        assert_eq!(
            replays(&dropped),
            0,
            "{changed} must drop the reasoning item"
        );
        // Staging drops it itself: the turn goes on without it.
        assert!(
            adapter(1).stage(switched(changed)?).is_ok(),
            "{changed} must not refuse the turn"
        );
    }
    Ok(())
}

#[test]
fn pc_reasoning_items_past_their_bound_or_malformed_are_not_retained(
) -> Result<(), Box<dyn std::error::Error>> {
    let binding = ReplayBinding::fixture();
    // The normalized item serializes to its fields plus the JSON around them.
    let framing = serde_json::json!({
        "type": "reasoning",
        "id": "rs_pc_1",
        "summary": [{"type": "summary_text", "text": "inspect the file"}],
        "encrypted_content": "",
    })
    .to_string()
    .len();
    let room = MAX_REASONING_ITEM_BYTES - framing;
    assert!(reasoning_state(&reasoning_item(&"e".repeat(room)), &binding).is_some());
    assert!(reasoning_state(&reasoning_item(&"e".repeat(room + 1)), &binding).is_none());
    let mut no_content = reasoning_item("x");
    no_content["encrypted_content"] = serde_json::Value::Null;
    let mut bad_summary = reasoning_item("x");
    bad_summary["summary"] = serde_json::json!([{"type": "other", "text": "x"}]);
    for item in [
        no_content,
        bad_summary,
        serde_json::json!({"type": "message"}),
    ] {
        assert!(reasoning_state(&item, &binding).is_none(), "{item}");
    }
    let state = reasoning_state(&reasoning_item("PC_ENCRYPTED_SENTINEL"), &binding)
        .ok_or("reasoning state")?;
    assert!(!format!("{state:?}").contains("PC_ENCRYPTED_SENTINEL"));
    Ok(())
}

#[test]
fn pc_empty_summary_parts_count_toward_the_reasoning_item_bound() {
    // Empty summary parts carry no text but still serialize as request bytes,
    // so enough of them exceed the bound with almost no encrypted content.
    let mut item = reasoning_item("x");
    item["summary"] = serde_json::Value::Array(vec![
        serde_json::json!({"type": "summary_text", "text": ""});
        MAX_REASONING_ITEM_BYTES / 32
    ]);
    assert!(reasoning_state(&item, &ReplayBinding::fixture()).is_none());
}

fn compaction_input() -> Result<AttemptInput, Box<dyn std::error::Error>> {
    let mut input = input(
        "pc-compaction",
        "http://127.0.0.1:1",
        ProtocolFamily::OpenAiResponses,
        crate::transport::fixtures::responses_request(),
    );
    let binding = ReplayBinding::for_compaction(&input).ok_or("binding")?;
    let state = compaction_state(
        &serde_json::json!({"type":"compaction","id":"cmp_pc_1","encrypted_content":"PC_COMPACTED"}),
        &binding,
    );
    let model = input.request.model();
    let replay = ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(state.capture_bytes().ok_or("state")?)?,
    )?;
    assert!(replay.is_responses_compaction());
    let ProviderRequest::OpenAiResponses(request) = &mut input.request else {
        return Err("family".into());
    };
    request.input.push(CodexInputItem::ProviderReplay(replay));
    Ok(input)
}

/// A provider compaction item is sent only on the route, account and model
/// that produced it; otherwise staging refuses rather than dropping context.
#[test]
fn pc_compaction_is_refused_on_any_other_route_account_or_model(
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(adapter(1).stage(compaction_input()?).is_ok());
    let mut account = compaction_input()?;
    account.lease = crate::CredentialLease::issue(
        grapt_types::AccountId::new("other-account").ok_or("account")?,
        account.lease.credential_version().clone(),
        crate::CredentialPlacement::BearerAuthorization,
        grapt_types::Secret::new("different-secret-never-hashed".to_owned()),
    )?;
    assert!(adapter(1).stage(account).is_err(), "account change");
    let mut model = compaction_input()?;
    let ProviderRequest::OpenAiResponses(request) = &mut model.request else {
        return Err("family".into());
    };
    request.model = crate::transport::fixtures::compat_request().model().clone();
    assert!(adapter(1).stage(model).is_err(), "model change");
    Ok(())
}

/// An oversized or malformed compaction item is never retained as usable;
/// each restores as its own named reason for the fallback record.
#[test]
fn pc_oversized_or_malformed_compaction_items_restore_as_named_reasons() {
    let binding = ReplayBinding::fixture();
    let model = crate::transport::fixtures::responses_request()
        .model()
        .clone();
    let restore = |item: serde_json::Value| {
        let state = compaction_state(&item, &binding);
        ProviderReplay::restore(
            model.provider().clone(),
            model.model().clone(),
            None,
            std::str::from_utf8(state.capture_bytes().unwrap_or_default()).unwrap_or_default(),
        )
        .err()
    };
    let big = "e".repeat(MAX_COMPACTION_ITEM_BYTES);
    assert_eq!(
        restore(serde_json::json!({"type":"compaction","id":"c","encrypted_content":big})),
        Some(ReplayError::CompactionOversize)
    );
    let huge = "e".repeat(AdapterState::MAX_BYTES);
    assert_eq!(
        restore(serde_json::json!({"type":"compaction","id":"c","encrypted_content":huge})),
        Some(ReplayError::CompactionOversize),
        "an item past the state bound is a marker, not a silent drop"
    );
    assert_eq!(
        restore(serde_json::json!({"type":"compaction","id":"c"})),
        Some(ReplayError::Invalid(COMPACTION_MALFORMED))
    );
    assert_eq!(
        restore(serde_json::json!({"type":"compaction","id":"c","encrypted_content":"ok"})),
        None
    );
}

/// The trigger is a Responses-only input item.
#[test]
fn pc_compaction_trigger_is_responses_only() -> Result<(), Box<dyn std::error::Error>> {
    let mut responses = crate::transport::fixtures::responses_request();
    let ProviderRequest::OpenAiResponses(request) = &mut responses else {
        return Err("family".into());
    };
    request.input.push(CodexInputItem::CompactionTrigger);
    assert!(crate::routes::request_validation::validate(&responses).is_ok());
    let ProviderRequest::OpenAiResponses(request) = &responses else {
        return Err("family".into());
    };
    assert_eq!(
        crate::routes::body::openai_responses_body(request)?["input"][1],
        serde_json::json!({"type":"compaction_trigger"})
    );
    let mut anthropic = anthropic_request();
    let ProviderRequest::AnthropicMessages(request) = &mut anthropic else {
        return Err("family".into());
    };
    request.messages.push(CodexInputItem::CompactionTrigger);
    assert!(crate::routes::request_validation::validate(&anthropic).is_err());
    Ok(())
}

#[test]
fn response_boundaries_preserve_signed_replay_bytes_and_original_binding(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut anthropic = bound_input()?;
    let ProviderRequest::AnthropicMessages(request) = &mut anthropic.request else {
        panic!("family")
    };
    let before = crate::routes::body::anthropic_body(request)?;
    request
        .messages
        .insert(1, CodexInputItem::AssistantResponseStart);
    assert_eq!(crate::routes::body::anthropic_body(request)?, before);
    assert!(adapter(1).stage(anthropic).is_ok());
    let mut responses = reasoning_input()?;
    let ProviderRequest::OpenAiResponses(request) = &mut responses.request else {
        panic!("family")
    };
    let before = crate::routes::body::openai_responses_body(request)?;
    request
        .input
        .insert(1, CodexInputItem::AssistantResponseStart);
    assert_eq!(crate::routes::body::openai_responses_body(request)?, before);
    assert!(adapter(1).stage(responses).is_ok());
    // Existing switch witnesses still exercise model/endpoint/account/version refusals.
    Ok(())
}

#[path = "native_search_replay_tests.rs"]
mod native_search_replay_tests;
#[test]
fn effort_compatibility_never_accepts_an_unknown_variant_identity(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut input = binding_input(true)?;
    let ProviderRequest::AnthropicMessages(request) = &mut input.request else {
        return Err("family".into());
    };
    let Some(CodexInputItem::ProviderReplay(replay)) = request.messages.last_mut() else {
        return Err("replay".into());
    };
    replay.variant = grapt_types::ModelVariantId::new("unrelated-generation-mode");
    drop_unreplayable_reasoning(&mut input);
    assert_eq!(replays(&input), 0);
    Ok(())
}

#[test]
fn advertised_request_effort_preserves_signatures_without_guessing_a_default(
) -> Result<(), Box<dyn std::error::Error>> {
    for (mode, accepted) in [
        (crate::EffortMode::Request, true),
        (crate::EffortMode::Unavailable, false),
    ] {
        let mut input = binding_input(true)?;
        let ProviderRequest::AnthropicMessages(request) = &mut input.request else {
            return Err("family".into());
        };
        request.effort = crate::EffortPolicy {
            mode,
            default: None,
        };
        drop_unreplayable_reasoning(&mut input);
        assert_eq!(replays(&input) == 1, accepted, "{mode:?}");
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn changed_compaction_effort_requires_a_supported_fresh_control_before_user_input(
) -> Result<(), Box<dyn std::error::Error>> {
    for case in [
        "fresh",
        "absent",
        "before",
        "late",
        "stale",
        "request",
        "unknown",
        "image-fresh",
        "image-late",
    ] {
        let ProviderRequest::OpenAiResponses(base) =
            crate::transport::fixtures::responses_request()
        else {
            return Err("Responses fixture".into());
        };
        let request = ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
            model: base.model,
            input: vec![],
            prompt_cache_key: None,
            tools: vec![],
            web_search: None,
            tool_choice: None,
            reasoning: crate::CodexReasoning {
                effort: crate::EffortPolicy {
                    mode: if case == "request" {
                        crate::EffortMode::Request
                    } else {
                        crate::EffortMode::Chronological
                    },
                    default: if case == "unknown" {
                        None
                    } else {
                        Some(crate::ReasoningEffort::High)
                    },
                },
                ..Default::default()
            },
            turn: Default::default(),
        });
        let mut input = input(
            "effort-compaction",
            "http://127.0.0.1:1",
            ProtocolFamily::ChatGptCodex,
            request,
        );
        let binding = ReplayBinding::for_compaction(&input).ok_or("compaction binding")?;
        let state = compaction_state(
            &serde_json::json!({"type":"compaction","id":"cmp_effort","encrypted_content":"COMPACTED"}),
            &binding,
        );
        let selected = input.request.model();
        let replay = ProviderReplay::restore(
            selected.provider().clone(),
            selected.model().clone(),
            Some(grapt_types::ModelVariantId::new("low").ok_or("effort")?),
            std::str::from_utf8(state.capture_bytes().ok_or("compaction state")?)?,
        )?;
        let ProviderRequest::ChatGptCodex(request) = &mut input.request else {
            return Err("Codex fixture".into());
        };
        if case == "before" {
            request
                .input
                .push(CodexInputItem::EffortUpdate(crate::ReasoningEffort::High));
        }
        request.input.push(CodexInputItem::ProviderReplay(replay));
        if matches!(
            case,
            "fresh" | "request" | "unknown" | "stale" | "image-fresh"
        ) {
            request
                .input
                .push(CodexInputItem::EffortUpdate(if case == "stale" {
                    crate::ReasoningEffort::Low
                } else {
                    crate::ReasoningEffort::High
                }));
        }
        request.input.push(if case.starts_with("image-") {
            CodexInputItem::ImageUser {
                text: crate::CodexToolText::try_new("continue".into())?,
                images: crate::InputImages::try_new(vec![crate::InputImage::try_new(
                    crate::ImageMedia::Png,
                    "YWJj".into(),
                    crate::ImageDetail::High,
                    800,
                    600,
                )?])?,
            }
        } else {
            crate::InputMessage {
                role: crate::MessageRole::User,
                content: "continue".into(),
            }
            .into()
        });
        if matches!(case, "late" | "image-late") {
            request
                .input
                .push(CodexInputItem::EffortUpdate(crate::ReasoningEffort::High));
        }
        assert_eq!(
            adapter(1).stage(input).is_ok(),
            matches!(case, "fresh" | "image-fresh"),
            "{case}"
        );
    }
    Ok(())
}

#[test]
fn switching_away_from_anthropic_drops_signed_thinking_and_keeps_the_turn(
) -> Result<(), Box<dyn std::error::Error>> {
    let ProviderRequest::AnthropicMessages(source) = bound_input()?.request else {
        return Err("family".into());
    };
    let mut other = crate::transport::fixtures::compat_request();
    let ProviderRequest::OpenAiCompatible(target) = &mut other else {
        return Err("family".into());
    };
    target.messages = source.messages;
    let mut switched = input(
        "pc-binding",
        "http://127.0.0.1:1",
        ProtocolFamily::OpenAiCompatible,
        other,
    );
    drop_unreplayable_reasoning(&mut switched);
    assert_eq!(replays(&switched), 0);
    assert!(adapter(1).stage(switched).is_ok());
    Ok(())
}
