//! CONTEXT-QUALITY-01 J02: calibrated local estimates.
//!
//! A calibration scales only locally estimated units by the same target's own
//! confirmed usage. It never exceeds the byte bound, never crosses a target,
//! and never replaces a confirmed prefix.

use super::*;
use grapt_provider::{InputMessage, MessageRole, PhysicalAttemptAuthoritySnapshot};

fn coding(content: &str) -> Result<ProviderRequest, String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let mut builder = PhysicalAttemptAuthoritySnapshot::builder();
    builder.model(target).map_err(|error| error.to_string())?;
    let authority = builder.build().map_err(|error| error.to_string())?;
    Ok(ProviderRequest::ChatGptCodex(
        grapt_provider::ChatGptCodexRequest {
            model: authority.model().model_ref().clone(),
            input: vec![CodexInputItem::Message(InputMessage {
                role: MessageRole::User,
                content: content.to_owned(),
            })],
            prompt_cache_key: None,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        },
    ))
}

fn appended(request: &ProviderRequest, content: String) -> Result<ProviderRequest, String> {
    let mut next = request.clone();
    let ProviderRequest::ChatGptCodex(codex) = &mut next else {
        return Err("fixture family".into());
    };
    codex.input.push(CodexInputItem::Message(InputMessage {
        role: MessageRole::Assistant,
        content,
    }));
    Ok(next)
}

fn attempt(name: &str) -> Result<grapt_types::PhysicalAttemptId, String> {
    grapt_types::PhysicalAttemptId::new(name).ok_or_else(|| "attempt".to_owned())
}

fn session(name: &str) -> Result<grapt_types::SessionId, String> {
    name.parse().map_err(|_| "session".to_owned())
}

/// A request the provider charged about half its window for was judged at
/// twice its confirmed count, so the window fit refused it and the warm
/// summary was over budget. The admitted count is the observed ratio itself;
/// the margin stays out of every judged number.
#[test]
fn cq_the_admitted_count_is_the_observed_ratio_not_the_packing_margin() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    // 216 500 local units, provider-confirmed at 106 000 tokens: about 0.49
    // tokens per unit, near the incident's confirmed-to-judged ratio.
    let request = coding(&"x".repeat(216_180))?;
    let units = estimate(&request).map_err(|error| format!("{error:?}"))?;
    assert_eq!(units, 216_500);
    const CONFIRMED: u64 = 106_000;
    const WINDOW: u64 = 200_000;

    let mut accounting = Accounting::default();
    accounting
        .estimate(&request, &target, None)
        .map_err(|error| format!("{error:?}"))?;
    accounting.admitted(&attempt("cq-f17")?);
    let counted = accounting
        .estimate(&request, &target, Some(CONFIRMED))
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(counted.tokens, CONFIRMED);

    // Half the window fits; the margin-doubled count the incident recorded
    // did not, and that refusal is the defect this witness holds closed.
    assert!(fits(counted.tokens, SUMMARY_OUTPUT_RESERVE, WINDOW));
    let packing_margin = Calibration::new(TargetKey::new(&target), CONFIRMED, units)
        .ok_or("calibration")?
        .charge(units)
        .map_err(|error| format!("{error:?}"))?;
    assert!(!fits(packing_margin, SUMMARY_OUTPUT_RESERVE, WINDOW));

    // The warm summary extends the admitted count, not the packing margin.
    let slot = DiagnosisSlot::default();
    slot.record_request(&request, 0)
        .map_err(|error| format!("{error:?}"))?;
    slot.record_calibration(Calibration::new(TargetKey::new(&target), CONFIRMED, units));
    slot.record_tokens(counted.tokens, Some(CONFIRMED));
    let warm = appended(&request, "summarize the conversation above".into())?;
    assert_eq!(slot.warm_summary_overflow(&warm, WINDOW), Ok(None));
    Ok(())
}

/// The incident's own numbers: a request the provider confirmed at 104 503
/// tokens in a 200 000-token window is 52% used. Judged at the observed ratio
/// it fits; the doubled count the incident recorded does not, which is why the
/// session compacted near half usage.
#[test]
fn cq_a_half_window_request_is_not_read_at_twice_its_confirmed_count() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    const CONFIRMED: u64 = 104_503;
    const WINDOW: u64 = 200_000;
    // The incident's local units for that request: about two units per charged
    // token, the ratio an English history with framing measures at.
    const UNITS: u64 = 209_006;
    let calibration =
        Calibration::new(TargetKey::new(&target), CONFIRMED, UNITS).ok_or("calibration")?;
    let judged = calibration.tokens(UNITS).map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        judged, CONFIRMED,
        "at the ratio, the count is what the provider charged"
    );
    assert!(
        fits(judged, SUMMARY_OUTPUT_RESERVE, WINDOW),
        "52% of the window fits with the summary reserve"
    );
    // The margin charges twice that ratio, capped at the uncalibrated
    // estimate. That doubled reading is what compacted the session, and it is
    // the number the incident logged as 213 254.
    let packed = calibration.charge(UNITS).map_err(|e| format!("{e:?}"))?;
    assert_eq!(packed, CONFIRMED * 2, "the margin's doubled reading");
    assert!(
        !fits(packed, SUMMARY_OUTPUT_RESERVE, WINDOW),
        "the incident's judged count did not fit, which is why it compacted"
    );
    Ok(())
}

/// 4064 content bytes plus framing: 4384 estimated units; 1200 confirmed
/// tokens is an ordinary 3.65 bytes per token.
const PRIOR_CONFIRMED: u64 = 1200;

fn prior() -> Result<ProviderRequest, String> {
    coding(&"x".repeat(4064))
}

#[test]
fn cq_the_suffix_is_judged_at_the_confirmed_ratio_and_never_above_bytes() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let prior = prior()?;
    let raw = estimate(&prior).map_err(|error| format!("{error:?}"))?;
    assert_eq!(raw, 4384);
    let mut accounting = Accounting::default();
    accounting
        .estimate(&prior, &target, None)
        .map_err(|error| format!("{error:?}"))?;
    accounting.admitted(&attempt("cq-prior")?);

    let small = appended(&prior, "done".into())?;
    // 68 suffix units at 1200/4384 tokens per unit, rounded up.
    assert_eq!(
        accounting.estimate(&small, &target, Some(PRIOR_CONFIRMED)),
        Ok(Estimate {
            tokens: PRIOR_CONFIRMED + 19,
            basis: EstimateBasis::ConfirmedPrefix,
        })
    );
    let large = appended(&prior, "y".repeat(40_000))?;
    let suffix = 40_064u64;
    let counted = accounting
        .estimate(&large, &target, Some(PRIOR_CONFIRMED))
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(
        counted.tokens,
        PRIOR_CONFIRMED + (suffix * PRIOR_CONFIRMED).div_ceil(raw)
    );
    assert!(
        counted.tokens < raw + suffix,
        "calibration did not lower the count"
    );

    // A ratio above one half would count more than a byte per unit; the
    // byte bound caps it. Here the provider confirmed fewer tokens than the
    // local estimate, so neither count reaches its units. The packing bound
    // still applies its margin, because SE is filling a budget there, not
    // judging a request.
    let dense = Calibration::new(TargetKey::new(&target), 3000, raw).ok_or("calibration")?;
    assert_eq!(dense.tokens(suffix), Ok((suffix * 3000).div_ceil(raw)));
    assert_eq!(dense.charge(suffix), Ok(suffix));
    assert_eq!(dense.tokens(0), Ok(0));
    assert_eq!(dense.charge(0), Ok(0));
    // A provider confirming more tokens than SE estimated caps both at the
    // units, so calibration never counts above the byte bound.
    let sparse = Calibration::new(TargetKey::new(&target), 4000, 1000).ok_or("calibration")?;
    assert_eq!(sparse.tokens(500), Ok(500));
    assert_eq!(sparse.charge(500), Ok(500));
    Ok(())
}

#[test]
fn cq_a_changed_prefix_is_calibrated_but_not_discounted() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let prior = prior()?;
    let mut accounting = Accounting::default();
    accounting
        .estimate(&prior, &target, None)
        .map_err(|error| format!("{error:?}"))?;
    accounting.admitted(&attempt("cq-rebuilt")?);
    // A compaction rebuild: the next request no longer starts with the prior.
    let rebuilt = coding(&"z".repeat(2_000))?;
    let full = estimate(&rebuilt).map_err(|error| format!("{error:?}"))?;
    let counted = accounting
        .estimate(&rebuilt, &target, Some(PRIOR_CONFIRMED))
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(counted.basis, EstimateBasis::Calibrated);
    assert_eq!(counted.tokens, (full * PRIOR_CONFIRMED).div_ceil(4384));
    assert_eq!(EstimateBasis::Calibrated.name(), "estimated");
    Ok(())
}

#[test]
fn cq_implausible_or_absent_usage_leaves_the_byte_bound() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let prior = prior()?;
    let rebuilt = coding(&"z".repeat(2_000))?;
    let full = estimate(&rebuilt).map_err(|error| format!("{error:?}"))?;
    for confirmed in [None, Some(0), Some(547)] {
        let mut accounting = Accounting::default();
        accounting
            .estimate(&prior, &target, None)
            .map_err(|error| format!("{error:?}"))?;
        accounting.admitted(&attempt("cq-implausible")?);
        assert_eq!(
            accounting.estimate(&rebuilt, &target, confirmed),
            Ok(Estimate {
                tokens: full,
                basis: EstimateBasis::Estimated,
            }),
            "{confirmed:?}"
        );
    }
    // 548 * 8 >= 4384: the smallest plausible report does calibrate.
    assert!(Calibration::new(TargetKey::new(&target), 548, 4384).is_some());
    assert!(Calibration::new(TargetKey::new(&target), 547, 4384).is_none());
    Ok(())
}

#[test]
fn cq_calibration_follows_its_session_and_target_into_the_next_execution() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let sessions = SessionCalibrations::default();
    let owner = session("ses_cq_calibrated")?;
    let prior = prior()?;
    let mut first = Accounting::for_session(sessions.clone(), owner.clone());
    first
        .estimate(&prior, &target, None)
        .map_err(|error| format!("{error:?}"))?;
    first.admitted(&attempt("cq-first-execution")?);
    first
        .estimate(
            &appended(&prior, "done".into())?,
            &target,
            Some(PRIOR_CONFIRMED),
        )
        .map_err(|error| format!("{error:?}"))?;

    // A later prompt opens a new execution with no prior request of its own.
    let fresh = coding(&"n".repeat(8_000))?;
    let full = estimate(&fresh).map_err(|error| format!("{error:?}"))?;
    let mut next = Accounting::for_session(sessions.clone(), owner);
    assert_eq!(
        next.estimate(&fresh, &target, None),
        Ok(Estimate {
            tokens: (full * PRIOR_CONFIRMED).div_ceil(4384),
            basis: EstimateBasis::Calibrated,
        })
    );
    // Another issuance of the target (a new model/route/account binding) and
    // another Session both start from the byte bound.
    let replacement = crate::dogfood::production::tests::projection_target()?;
    assert_eq!(
        next.estimate(&fresh, &replacement, None).map(|e| e.basis),
        Ok(EstimateBasis::Estimated)
    );
    let mut other = Accounting::for_session(sessions, session("ses_cq_other")?);
    assert_eq!(
        other.estimate(&fresh, &target, None).map(|e| e.basis),
        Ok(EstimateBasis::Estimated)
    );
    Ok(())
}

#[test]
fn cq_session_calibrations_stay_bounded() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let sessions = SessionCalibrations::default();
    for index in 0..=MAX_CALIBRATED_SESSIONS {
        let calibration =
            Calibration::new(TargetKey::new(&target), 1200, 4384).ok_or("calibration")?;
        sessions.update(&session(&format!("ses_cq_bound_{index:04}"))?, |shared| {
            shared.calibration = Some(calibration)
        });
    }
    assert_eq!(
        super::super::lock(&sessions.0).len(),
        MAX_CALIBRATED_SESSIONS
    );
    assert!(sessions
        .get(&session(&format!(
            "ses_cq_bound_{MAX_CALIBRATED_SESSIONS:04}"
        ))?)
        .0
        .is_some());
    Ok(())
}

/// A 128 000-token window: input fits below this with the 16 384 output
/// reserve and SE's headroom.
const WINDOW: u64 = 128_000;
/// About 60% of that window, confirmed by the provider for a 280 000-byte
/// request: 3.65 bytes per token, as in [`PRIOR_CONFIRMED`].
const LONG_CONFIRMED: u64 = 76_800;

/// An execution whose last admitted request was `long` on `target`.
fn long_session(
    sessions: &SessionCalibrations,
    owner: &grapt_types::SessionId,
    target: &grapt_types::ResolvedModelTarget,
    long: &ProviderRequest,
) -> Result<(), String> {
    let mut first = Accounting::for_session(sessions.clone(), owner.clone());
    first
        .estimate(long, target, None)
        .map_err(|error| format!("{error:?}"))?;
    first.admitted(&attempt("cq-long-execution")?);
    Ok(())
}

/// The next prompt opens a new execution. Its first request repeats the last
/// one exactly and adds the answer and the new input, so only that addition
/// is estimated. A session at 60% of its window must not compact: the request
/// is judged at the ratio the provider charged for the prefix it repeats.
#[test]
fn cq_a_new_execution_is_counted_from_the_confirmed_count_of_the_request_it_repeats(
) -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let sessions = SessionCalibrations::default();
    let owner = session("ses_cq_seeded")?;
    let long = coding(&"x".repeat(280_000))?;
    let prior = estimate(&long).map_err(|error| format!("{error:?}"))?;
    long_session(&sessions, &owner, &target, &long)?;

    let mut next = Accounting::for_session(sessions, owner);
    assert_eq!(
        next.previous_attempt(),
        Some(&attempt("cq-long-execution")?),
        "the new execution must find the last admitted attempt"
    );
    let request = appended(&appended(&long, "done".into())?, "n".repeat(2_000))?;
    let full = estimate(&request).map_err(|error| format!("{error:?}"))?;
    let seeded = next
        .estimate(&request, &target, Some(LONG_CONFIRMED))
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(seeded.basis, EstimateBasis::ConfirmedPrefix);
    assert_eq!(
        seeded.tokens,
        LONG_CONFIRMED + ((full - prior) * LONG_CONFIRMED).div_ceil(prior)
    );
    let reserve = output_reserve(&request);
    assert!(fits(seeded.tokens, reserve, WINDOW));
    // The packing margin on the whole request would have compacted this
    // session; the judged count does not. That difference is the defect this
    // seam closes, so the witness holds both sides of it.
    let packed = Calibration::new(TargetKey::new(&target), LONG_CONFIRMED, prior)
        .ok_or("calibration")?
        .charge(full)
        .map_err(|error| format!("{error:?}"))?;
    assert!(!fits(packed, reserve, WINDOW));
    Ok(())
}

/// When the new request does not repeat the last one, the prior confirmed
/// count no longer describes a prefix: the estimate falls back to the
/// calibrated whole request.
fn falls_back_to_calibration(label: &str, request: ProviderRequest) -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let long = coding(&"x".repeat(280_000))?;
    let prior = estimate(&long).map_err(|error| format!("{error:?}"))?;
    let sessions = SessionCalibrations::default();
    let owner = session("ses_cq_fallback")?;
    long_session(&sessions, &owner, &target, &long)?;
    let mut next = Accounting::for_session(sessions, owner);
    let full = estimate(&request).map_err(|error| format!("{error:?}"))?;
    assert_eq!(
        next.estimate(&request, &target, Some(LONG_CONFIRMED)),
        Ok(Estimate {
            tokens: (full * LONG_CONFIRMED).div_ceil(prior).min(full),
            basis: EstimateBasis::Calibrated,
        }),
        "{label}"
    );
    Ok(())
}

/// A compaction replaced the history with its summary.
#[test]
fn cq_a_new_execution_after_compaction_falls_back_to_calibration() -> Result<(), String> {
    falls_back_to_calibration(
        "compacted",
        appended(&coding("summary of the earlier work")?, "n".repeat(2_000))?,
    )
}

/// An earlier message was edited or reverted: the same length, one byte
/// different.
#[test]
fn cq_a_new_execution_after_an_edit_falls_back_to_calibration() -> Result<(), String> {
    falls_back_to_calibration(
        "edited",
        appended(
            &coding(&format!("y{}", "x".repeat(279_999)))?,
            "n".repeat(2_000),
        )?,
    )
}

#[test]
fn cq_output_reserve_is_the_bound_the_request_actually_asks_for() -> Result<(), String> {
    let codex = prior()?;
    assert_eq!(output_reserve(&codex), OUTPUT_RESERVE);
    let ProviderRequest::ChatGptCodex(template) = codex else {
        return Err("fixture family".into());
    };
    let bound = |value| grapt_provider::MaxOutputTokens::new(value).ok_or("bound");
    let anthropic = ProviderRequest::AnthropicMessages(grapt_provider::AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: grapt_provider::EffortPolicy::default(),
        model: template.model.clone(),
        messages: template.input.clone(),
        max_tokens: bound(8192)?,
        instruction_mode: grapt_provider::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: grapt_provider::AnthropicPromptCache::StablePrefix,
        tools: Vec::new(),
        tool_choice: None,
    });
    assert_eq!(output_reserve(&anthropic), 8192);
    let before_suffix = estimate(&anthropic).map_err(|error| format!("{error:?}"))?;
    let mut with_suffix = anthropic.clone();
    let ProviderRequest::AnthropicMessages(request) = &mut with_suffix else {
        return Err("fixture family".into());
    };
    request.uncached_system_suffix = Some("Session ID: cache04".into());
    assert_eq!(
        estimate(&with_suffix).map_err(|error| format!("{error:?}"))?,
        before_suffix + "Session ID: cache04".len() as u64 + 64
    );
    for (max, reserve) in [(None, OUTPUT_RESERVE), (Some(bound(1000)?), 1000)] {
        let responses = ProviderRequest::OpenAiResponses(grapt_provider::OpenAiResponsesRequest {
            model: template.model.clone(),
            input: template.input.clone(),
            prompt_cache_key: None,
            max_output_tokens: max,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
        });
        assert_eq!(output_reserve(&responses), reserve);
        let compatible =
            ProviderRequest::OpenAiCompatible(grapt_provider::OpenAiCompatibleRequest {
                reasoning_replay: Default::default(),
                prompt_cache: grapt_provider::CompatiblePromptCache::Off,
                model: template.model.clone(),
                messages: template.input.clone(),
                max_tokens: max,
                tools: Vec::new(),
                tool_choice: None,
                routing_session: None,
            });
        assert_eq!(output_reserve(&compatible), reserve);
    }
    Ok(())
}

/// A rebuilt request is content SE sizes against the window, so it keeps the
/// conservative packing bound [`DiagnosisSlot::summary_bytes`] inverts.
#[test]
fn cq_rebuilt_request_is_judged_by_the_packing_calibration() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let request = prior()?;
    let slot = DiagnosisSlot::default();
    slot.record_request(&request, 0)
        .map_err(|error| format!("{error:?}"))?;
    let history = vec![CodexInputItem::Message(InputMessage {
        role: MessageRole::Assistant,
        content: "s".repeat(10_000),
    })];
    let full = 256 + 10_000 + 64;
    assert_eq!(
        slot.rebuilt_estimate(&history),
        Ok(Some((full, OUTPUT_RESERVE)))
    );
    slot.record_calibration(Calibration::new(TargetKey::new(&target), 1200, 4384));
    assert_eq!(
        slot.rebuilt_estimate(&history),
        Ok(Some(((full * 2400).div_ceil(4384), OUTPUT_RESERVE)))
    );
    Ok(())
}

/// Replay the calibrated rule over the pinned cohort corpus: every coding
/// request's provider-measured input, against what admission would charge.
#[test]
fn cq_pinned_corpus_is_never_under_counted_and_is_counted_closer() -> Result<(), String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/context-quality/estimator-corpus.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("pinned corpus {} is required: {error}", path.display()));
    let corpus: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    assert_eq!(corpus["schema"], "context-quality-estimator-corpus/v1");
    let target = crate::dogfood::production::tests::projection_target()?;
    let (mut before, mut after) = (Vec::new(), Vec::new());
    let mut judged_ratio = Vec::new();
    for (case, rows) in corpus["cases"].as_object().ok_or("cases")? {
        let mut previous: Option<(u64, u64)> = None;
        for row in rows.as_array().ok_or("rows")? {
            if row[1] != "coding" {
                continue;
            }
            let (Some(estimated), Some(measured)) = (row[2].as_u64(), row[3].as_u64()) else {
                continue;
            };
            let exact = row[4].as_bool() == Some(true);
            let prior = row[5].as_u64();
            let calibration = previous.and_then(|(estimated, measured)| {
                Calibration::new(TargetKey::new(&target), measured, estimated)
            });
            let (old, charged, judged) = match (exact, previous, prior, &calibration) {
                (true, Some((prior_estimate, _)), Some(prior), Some(calibration)) => {
                    let suffix = estimated.checked_sub(prior_estimate).ok_or("suffix")?;
                    let charged = calibration.charge(suffix).map_err(|e| format!("{e:?}"))?;
                    let judged = calibration.tokens(suffix).map_err(|e| format!("{e:?}"))?;
                    (prior + suffix, prior + charged, prior + judged)
                }
                (_, _, _, Some(calibration)) => {
                    let charged = calibration
                        .charge(estimated)
                        .map_err(|e| format!("{e:?}"))?;
                    let judged = calibration
                        .tokens(estimated)
                        .map_err(|e| format!("{e:?}"))?;
                    (estimated, charged, judged)
                }
                _ => (estimated, estimated, estimated),
            };
            // The packing bound is the conservative one: it never under-counts,
            // so content SE fills a budget with cannot overflow it.
            assert!(
                charged >= measured,
                "{case} ordinal {}: packed {charged} below measured {measured}",
                row[0]
            );
            // The judging count is never above the packing bound and never
            // above the uncalibrated estimate.
            assert!(
                judged <= charged,
                "{case} ordinal {}: judged {judged} above packed {charged}",
                row[0]
            );
            assert!(
                judged <= old,
                "{case} ordinal {}: calibration raised a count",
                row[0]
            );
            before.push(old as f64 / measured as f64);
            after.push(charged as f64 / measured as f64);
            judged_ratio.push(judged as f64 / measured as f64);
            previous = Some((estimated, measured));
        }
    }
    assert_eq!(after.len(), 222, "corpus coding rows");
    let median = |ratios: &mut Vec<f64>| {
        ratios.sort_by(f64::total_cmp);
        ratios[ratios.len() / 2]
    };
    let (old, new, judged) = (
        median(&mut before),
        median(&mut after),
        median(&mut judged_ratio),
    );
    println!(
        "corpus count/measured median: byte-bound {old:.3}, packed {new:.3}, judged {judged:.3}"
    );
    assert!(new < old, "calibration did not tighten the median");
    // Judging at the observed ratio is closer to what the provider charged
    // than the conservative packing bound, which is what admission reads.
    assert!(judged < new, "the judging count did not tighten the median");
    Ok(())
}

/// SC-COMPACT-01: the summary history is sized by inverting the calibrated
/// charge, so a payload that fills it is still admitted, and an uncalibrated
/// window keeps one byte per token.
#[test]
fn a_summary_history_fills_what_the_calibrated_charge_admits() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    // 1000 confirmed tokens for 3500 estimated bytes: 3.5 bytes per token.
    let calibration = Calibration::new(TargetKey::new(&target), 1000, 3500).ok_or("calibration")?;
    for tokens in [1u64, 4096, 483_616, 1_000_000] {
        let bytes = calibration.within(tokens);
        let charged = calibration.charge(bytes).map_err(|e| format!("{e:?}"))?;
        assert!(
            charged <= tokens,
            "{bytes} bytes charge {charged} over {tokens}"
        );
        let over = calibration
            .charge(bytes + 2)
            .map_err(|e| format!("{e:?}"))?;
        assert!(
            over > tokens || bytes == tokens,
            "{bytes} bytes leave the window unfilled"
        );
    }
    assert_eq!(calibration.within(483_616), 846_328);
    let dense = Calibration::new(TargetKey::new(&target), 3000, 3000).ok_or("calibration")?;
    assert_eq!(
        dense.within(10_000),
        10_000,
        "a history is never held below a byte per token"
    );
    let slot = DiagnosisSlot::default();
    assert_eq!(
        slot.summary_bytes(500_000),
        500_000 - super::super::summarization::SUMMARY_RESERVE
    );
    Ok(())
}

/// SC-COMPACT-01 fix 2: a structural refusal plans no request, so it stands
/// on the request the previous plan sent, with that plan's calibration; on
/// row A its absence sent every compaction to the cold, uncached summary.
#[test]
fn a_structural_refusal_reuses_only_the_request_last_sent() -> Result<(), String> {
    let target = crate::dogfood::production::tests::projection_target()?;
    let sent = coding("the request the provider received")?;
    let calibration = Calibration::new(TargetKey::new(&target), 1000, 3500).ok_or("calibration")?;
    let slot = DiagnosisSlot::default();

    // A plan records its request and calibration; the attempt is sent.
    slot.clear();
    slot.record_request(&sent, 0)
        .map_err(|e| format!("{e:?}"))?;
    slot.record_calibration(Some(calibration.clone()));
    slot.sent();
    // The next plan refuses at a structural bound before planning a request.
    slot.clear();
    assert!(slot.coding_request().is_none());
    slot.reuse_previous_request();
    assert_eq!(slot.coding_request(), Some(sent.clone()));
    assert_eq!(slot.summary_bytes(1_016_384), calibration.within(1_000_000));
    // The plan after that compaction never sees it again.
    slot.clear();
    slot.reuse_previous_request();
    assert!(
        slot.coding_request().is_none(),
        "a reused request outlived its refusal"
    );

    // A request refused at the threshold was never sent: it is not reused.
    slot.clear();
    slot.record_request(&coding("refused at the threshold")?, 0)
        .map_err(|e| format!("{e:?}"))?;
    slot.clear();
    slot.reuse_previous_request();
    assert!(
        slot.coding_request().is_none(),
        "an unsent request was reused"
    );

    // A plan that records its own request keeps it.
    slot.clear();
    slot.record_request(&sent, 0)
        .map_err(|e| format!("{e:?}"))?;
    slot.sent();
    slot.clear();
    let own = coding("this plan's own request")?;
    slot.record_request(&own, 0).map_err(|e| format!("{e:?}"))?;
    slot.reuse_previous_request();
    assert_eq!(slot.coding_request(), Some(own));
    Ok(())
}
