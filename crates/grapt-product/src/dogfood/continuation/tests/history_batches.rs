//! BR-H product projections: the shared reader preserves the individual view.
use super::*;

fn compare(continuation: &DurableContinuation, targets: &[ToolCallId]) -> Result<(), String> {
    let mut shared = ToolProjection::read_batches(
        &continuation.inbox,
        &continuation.artifacts,
        &continuation.session,
        targets,
    );
    for target in targets {
        let actual = shared
            .remove(target)
            .ok_or("selected projection disappeared")?;
        let expected = tool_projection(
            &continuation.inbox,
            &continuation.artifacts,
            &continuation.session,
            target,
        );
        match (actual, expected) {
            (Ok(actual), Ok(expected)) => {
                assert_eq!(actual.never_ran, expected.never_ran);
                for terminal in [false, true] {
                    assert_eq!(
                        crate::dogfood::history_json::tool_state_json(
                            &actual,
                            &continuation.artifacts,
                            terminal
                        )?,
                        crate::dogfood::history_json::tool_state_json(
                            &expected,
                            &continuation.artifacts,
                            terminal
                        )?,
                    );
                }
            }
            (Err(actual), Err(expected)) => assert_eq!(actual, expected),
            (actual, expected) => {
                return Err(format!(
                    "batch differs from the individual projection: {actual:?} versus {expected:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn br_h_batch_keeps_outcome_arguments_withholding_capture_and_restart_truth() -> Result<(), String>
{
    // (settled, failed before permission, interrupted, arguments, output bytes)
    for (index, (settled, failed, interrupted, arguments, bytes)) in [
        (true, false, false, "{}", 12),
        (false, false, true, "{}", 0),
        (true, true, false, "{}", 0),
        (true, false, false, "[1]", 12),
        (true, false, false, "", 64 * 1024),
        (
            true,
            false,
            false,
            "{}",
            MAX_BLOCK_CAPTURE_BYTES as usize + 1,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut fixture = Fixture::new(&format!("history-batch-{index}"))?;
        fixture.record_exchange_call(
            (settled, failed, interrupted),
            READ_TOOL,
            "read",
            &vec![b'x'; bytes],
            arguments,
        )?;
        let target = ToolCallId::new("call_d2a_read").ok_or("fixture call")?;
        let input = fixture.input.clone();
        for restarted in [false, true] {
            let continuation = if restarted {
                fixture.close();
                fixture.reopen(input.clone())?
            } else {
                fixture.continuation(input.clone())?
            };
            compare(&continuation, std::slice::from_ref(&target))?;
            let mut page = ToolProjection::read_batch(
                &continuation.inbox,
                &continuation.artifacts,
                &continuation.session,
                std::iter::once(&target),
            );
            let projection = page.remove(&target).ok_or("projection omitted")?;
            if bytes > MAX_BLOCK_CAPTURE_BYTES as usize {
                assert!(
                    projection.is_err(),
                    "truncated capture became a readable outcome"
                );
            } else {
                let projection = projection?;
                assert_eq!(projection.settled, settled);
                assert_eq!(projection.never_ran, !settled && !failed);
                if !settled {
                    let state = crate::dogfood::history_json::tool_state_json(
                        &projection,
                        &continuation.artifacts,
                        true,
                    )?;
                    assert!(
                        state.contains("permission was never granted"),
                        "withheld effect became an unknown or completed effect: {state}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn br_h_batch_retains_nested_progress_files_order_and_restart() -> Result<(), String> {
    let (mut fixture, old, current) = super::accumulation::accumulated(true, true)?;
    let targets = [old, current];
    let input = fixture.input.clone();
    for restarted in [false, true] {
        let continuation = if restarted {
            fixture.close();
            fixture.reopen(input.clone())?
        } else {
            fixture.continuation(input.clone())?
        };
        compare(&continuation, &targets)?;
        let shared = ToolProjection::read_batch(
            &continuation.inbox,
            &continuation.artifacts,
            &continuation.session,
            targets.iter(),
        );
        for (index, target) in targets.iter().enumerate() {
            let projection = shared
                .get(target)
                .ok_or("nested projection omitted")?
                .as_ref()
                .map_err(Clone::clone)?;
            assert_eq!(projection.files.len(), 1);
            assert_eq!(
                projection
                    .progress
                    .as_ref()
                    .ok_or("nested progress absent")?
                    .calls()
                    .len(),
                if index == 0 { 64 } else { 2 }
            );
        }
    }
    Ok(())
}

#[test]
fn br_h_batch_does_not_retry_a_failed_store_into_absence() -> Result<(), String> {
    let fixture = Fixture::new("history-batch-store-error")?;
    fixture.record_exchange()?;
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    raw.execute_batch("ALTER TABLE event RENAME TO unreadable_event")
        .map_err(|error| error.to_string())?;
    let targets = [
        ToolCallId::new("call_d2a_read").ok_or("call")?,
        ToolCallId::new("call_absent").ok_or("call")?,
    ];
    let projections = ToolProjection::read_batch(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        targets.iter(),
    );
    assert_eq!(projections.len(), targets.len());
    for target in &targets {
        let error = projections
            .get(target)
            .ok_or("failed store omitted a projection")?
            .as_ref()
            .err()
            .ok_or("failed store became a successful projection")?;
        assert!(
            error.contains("no such table"),
            "the read failure lost its evidence: {error}"
        );
    }
    Ok(())
}

#[test]
fn br_h_public_batch_preserves_eight_views_with_comparable_work_measurements() -> Result<(), String>
{
    let (fixture, _, _) = super::accumulation::accumulated(true, false)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let targets = (0..8)
        .map(|index| ToolCallId::new(format!("prior-{index}")).ok_or("target"))
        .collect::<Result<Vec<_>, _>>()?;
    compare(&continuation, &targets)?;
    let mut nanos = [0u128; 2];
    // Alternate the order on the same durable state; no timing threshold makes
    // this a flaky correctness gate. The VM witness supplies a deterministic bar.
    for sample in 0..16 {
        for variant in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let started = tokio::time::Instant::now();
            if variant == 0 {
                for target in &targets {
                    let projection = tool_projection(
                        &continuation.inbox,
                        &continuation.artifacts,
                        &continuation.session,
                        target,
                    )?;
                    assert!(projection.settled);
                }
            } else {
                let page = ToolProjection::read_batch(
                    &continuation.inbox,
                    &continuation.artifacts,
                    &continuation.session,
                    targets.iter(),
                );
                assert_eq!(page.len(), targets.len());
                for projection in page.values() {
                    assert!(projection.as_ref().map_err(Clone::clone)?.settled);
                }
            }
            let elapsed = started.elapsed().as_nanos();
            assert!(elapsed > 0, "public-reader measurement is missing");
            nanos[variant] += elapsed;
        }
    }
    eprintln!(
        "J01_PUBLIC_BATCH {}",
        serde_json::json!({"samples":16,"targets":8,"individual_ns":nanos[0],"batched_ns":nanos[1]})
    );
    Ok(())
}

/// SC-HIST-01: a render batch with more calls than one shared read selects
/// projects every call through shared reads, in groups, up to the group cap.
/// Before, every call past the first eight fell back to its own projection.
#[test]
fn a_render_batch_past_one_group_projects_every_call_through_shared_reads() -> Result<(), String> {
    let (fixture, _, _) = super::accumulation::accumulated(true, false)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let targets = |count: usize| {
        (0..count)
            .map(|index| ToolCallId::new(format!("prior-{index}")).ok_or("target"))
            .collect::<Result<Vec<_>, _>>()
    };
    // Three groups: two full, one partial. Each view equals its own projection.
    compare(&continuation, &targets(17)?)?;
    let capped = targets(65)?;
    let shared = ToolProjection::read_batch(
        &continuation.inbox,
        &continuation.artifacts,
        &continuation.session,
        capped.iter(),
    );
    assert_eq!(
        shared.len(),
        64,
        "the group cap no longer bounds one render batch"
    );
    assert!(!shared.contains_key(&capped[64]));
    Ok(())
}

#[test]
fn br_h_grouped_calls_cross_the_eight_call_bound_without_individual_fallback() -> Result<(), String>
{
    let (fixture, _, _) = super::accumulation::accumulated(true, false)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    for count in [0, 1, 8, 9, 17] {
        let targets = (0..count)
            .map(|index| ToolCallId::new(format!("prior-{index}")).ok_or("target"))
            .collect::<Result<Vec<_>, _>>()?;
        let projections = ToolProjection::read_batches(
            &continuation.inbox,
            &continuation.artifacts,
            &continuation.session,
            &targets,
        );
        assert_eq!(
            projections.len(),
            count,
            "a selected call fell back to a scalar read"
        );
        compare(&continuation, &targets)?;
    }
    Ok(())
}
