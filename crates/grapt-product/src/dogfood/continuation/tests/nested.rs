use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn final_files_follow_call_order_and_require_sensitive_complete_artifacts() -> Result<(), String> {
    for sensitivity in [Sensitivity::SecretBearing, Sensitivity::Ordinary] {
        let fixture = Fixture::new("final-files")?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(fixture.inbox()?),
            open_artifacts(&fixture.artifact_root)?,
            Arc::new(FixedClock),
            Arc::new(CountingIds::default()),
            NoLiveSubscribers,
            fixture.session.clone(),
            StepNumber::new(1).ok_or("step")?,
            None,
        )
        .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new("files-attempt").ok_or("attempt")?,
            })
            .map_err(|error| error.to_string())?;
        let parent = ToolCallId::new("files-parent").ok_or("parent")?;
        let mut outcomes = Vec::new();
        let mut references = Vec::new();
        for index in 0..2 {
            let call = ToolCallId::new(format!("files-child-{index}")).ok_or("call")?;
            owner
                .commit(DurableFact::NestedToolCalled {
                    parent: parent.clone(),
                    call: call.clone(),
                    tool: grapt_types::ToolDefinitionId::new("image").ok_or("tool")?,
                    catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                    input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                        .map_err(|error| error.to_string())?,
                })
                .map_err(|error| error.to_string())?;
            let binary = fixture
                .artifacts()?
                .capture(
                    Cursor::new([0xff, index]),
                    CaptureBound::new(1024, 1024).ok_or("bound")?,
                    ArtifactKind::CapturedOutput,
                    sensitivity,
                )
                .map_err(|error| error.to_string())?;
            let reference = binary.reference();
            let envelope = serde_json::json!({"value":{"private":"hidden"},"artifacts":[{
                "id":reference.id().as_str(),"digest":reference.digest().as_str(),"length":reference.length(),
                "mimeType":"image/png","sensitivity":"secret-bearing"}]});
            references.push(reference.clone());
            let captured = fixture
                .artifacts()?
                .capture(
                    Cursor::new(envelope.to_string().into_bytes()),
                    CaptureBound::new(4096, 4096).ok_or("bound")?,
                    ArtifactKind::CapturedOutput,
                    Sensitivity::SecretBearing,
                )
                .map_err(|error| error.to_string())?;
            outcomes.push(DurableFact::ToolTerminal {
                call,
                outcome: ToolTerminalOutcome::Completed {
                    output: Some(
                        ToolTerminalArtifact::new(
                            captured.reference().clone(),
                            captured.disposition(),
                        )
                        .ok_or("output")?,
                    ),
                },
            });
        }
        for outcome in outcomes.into_iter().rev() {
            owner.commit(outcome).map_err(|error| error.to_string())?;
        }
        let inbox = lock(fixture.inbox()?);
        let collected = files::collect(
            inbox.store(),
            fixture.artifacts()?,
            &fixture.session,
            &parent,
        );
        if sensitivity == Sensitivity::Ordinary {
            assert!(collected.is_err());
            continue;
        }
        let files = collected?;
        assert_eq!(files.len(), 2);
        for (file, reference) in files.iter().zip(&references) {
            assert_eq!(&file.reference, reference);
            let content = files::content(file, fixture.artifacts()?)?;
            assert!(content.starts_with("{\"type\":\"file\""));
            assert!(!content.contains("hidden"));
        }
        let missing = files::FinalFile {
            reference: grapt_types::ArtifactRef::new(
                grapt_types::ArtifactId::new("art_missing").ok_or("missing id")?,
                references[0].length(),
                references[0].digest().clone(),
            ),
            mime: "image/png".to_owned(),
        };
        assert!(files::content(&missing, fixture.artifacts()?).is_err());
        assert!(files::collect(
            inbox.store(),
            fixture.artifacts()?,
            &fixture.session,
            &ToolCallId::new("other-parent").ok_or("other")?
        )?
        .is_empty());
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn missing_nested_results_quarantine_continuation_but_unknown_ones_admit_it() -> Result<(), String>
{
    for unknown in [false, true] {
        let fixture = Fixture::new(if unknown {
            "nested-unknown"
        } else {
            "nested-recorded"
        })?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(fixture.inbox()?),
            open_artifacts(&fixture.artifact_root)?,
            Arc::new(FixedClock),
            Arc::new(CountingIds::default()),
            NoLiveSubscribers,
            fixture.session.clone(),
            StepNumber::new(1).ok_or("step")?,
            None,
        )
        .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new("quarantine-attempt").ok_or("attempt")?,
            })
            .map_err(|error| error.to_string())?;
        let call = ToolCallId::new("quarantine-child").ok_or("call")?;
        let parent = ToolCallId::new("execute-parent").ok_or("parent")?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: call.clone(),
                tool: grapt_types::ToolDefinitionId::new("effect").ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|error| error.to_string())?,
            })
            .map_err(|error| error.to_string())?;
        {
            let inbox = lock(fixture.inbox()?);
            assert!(require_resolved_effects(inbox.store(), &fixture.session).is_err());
            let progress = progress::snapshot(inbox.store(), &fixture.session, &parent, true)?
                .ok_or("committed child progress absent")?;
            assert_eq!(
                progress.calls()[0].status(),
                grapt_api::u3a::ToolProgressStatus::Running
            );
            assert!(stored_outputs(
                inbox.store(),
                &fixture.session,
                fixture.artifacts()?,
                None,
                None,
            )?
            .is_empty());
        }
        owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: if unknown {
                    ToolTerminalOutcome::Failed {
                        reason: grapt_types::ToolFailureReason::EffectUnknown,
                        detail: None,
                    }
                } else {
                    ToolTerminalOutcome::Completed { output: None }
                },
            })
            .map_err(|error| error.to_string())?;
        {
            let inbox = lock(fixture.inbox()?);
            let progress = progress::snapshot(inbox.store(), &fixture.session, &parent, true)?
                .ok_or("committed terminal progress absent")?;
            assert_eq!(
                progress.calls()[0].status(),
                if unknown {
                    grapt_api::u3a::ToolProgressStatus::Error
                } else {
                    grapt_api::u3a::ToolProgressStatus::Completed
                }
            );
        }
        owner
            .commit(DurableFact::ToolTerminal {
                call: parent.clone(),
                outcome: ToolTerminalOutcome::Completed { output: None },
            })
            .map_err(|error| error.to_string())?;
        drop(owner);
        let inbox = lock(fixture.inbox()?);
        assert!(progress::snapshot(inbox.store(), &fixture.session, &parent, true)?.is_none());
        assert!(progress::snapshot(inbox.store(), &fixture.session, &parent, false)?.is_some());
        // Both now carry a terminal; an unknown one is warned, not quarantined.
        require_resolved_effects(inbox.store(), &fixture.session)?;
        let notices = super::super::unknown_effects::notices(
            inbox.store(),
            &fixture.session,
            u64::MAX,
            &std::collections::BTreeSet::new(),
        )?;
        assert_eq!(notices.len(), usize::from(unknown));
        if let Some((_, CodexInputItem::Message(message))) = notices.first() {
            assert!(message
                .content
                .starts_with("[Harness notice: Tool effect (call quarantine-child, run inside execute-parent) ended with an unknown effect."));
        }
        let outputs = stored_outputs(
            inbox.store(),
            &fixture.session,
            fixture.artifacts()?,
            None,
            None,
        )?;
        assert_eq!(outputs[0].call, parent);
        assert_eq!(
            outputs.len(),
            1,
            "only the settled parent reaches model history"
        );
    }
    Ok(())
}

#[test]
fn nested_binary_result_never_enters_model_text_fetch() -> Result<(), String> {
    for is_nested in [true, false] {
        let fixture = Fixture::new(if is_nested {
            "nested-binary"
        } else {
            "direct-binary"
        })?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(fixture.inbox()?),
            open_artifacts(&fixture.artifact_root)?,
            Arc::new(FixedClock),
            Arc::new(CountingIds::default()),
            NoLiveSubscribers,
            fixture.session.clone(),
            StepNumber::new(1).ok_or("step")?,
            None,
        )
        .map_err(|error| error.to_string())?;
        let call = ToolCallId::new("binary-child").ok_or("call")?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new("binary-attempt").ok_or("attempt")?,
            })
            .map_err(|error| error.to_string())?;
        if is_nested {
            owner
                .commit(DurableFact::NestedToolCalled {
                    parent: ToolCallId::new("execute-parent").ok_or("parent")?,
                    call: call.clone(),
                    tool: grapt_types::ToolDefinitionId::new("image").ok_or("tool")?,
                    catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                    input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                        .map_err(|error| error.to_string())?,
                })
                .map_err(|error| error.to_string())?;
        }
        let captured = fixture
            .artifacts()?
            .capture(
                Cursor::new([0xff, 0xfe]),
                CaptureBound::new(1024, 1024).ok_or("bound")?,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: ToolTerminalOutcome::Completed {
                    output: Some(
                        ToolTerminalArtifact::new(
                            captured.reference().clone(),
                            captured.disposition(),
                        )
                        .ok_or("artifact")?,
                    ),
                },
            })
            .map_err(|error| error.to_string())?;
        let inbox = lock(fixture.inbox()?);
        let result = stored_outputs(
            inbox.store(),
            &fixture.session,
            fixture.artifacts()?,
            None,
            None,
        );
        if is_nested {
            assert!(result?.is_empty());
        } else {
            let outputs = result?;
            let output = outputs
                .first()
                .ok_or("direct model output projection is absent")?
                .output
                .as_str();
            assert!(output.contains('�'));
            assert!(output.ends_with(
                "[tool output projection: invalid UTF-8 bytes were replaced explicitly]"
            ));
        }
    }
    Ok(())
}

/// Review HOLD on 8e584dcc: shown unknown effects never crowd an older,
/// unshown one out of the 16-notice bound, and the overflow marker appears
/// only when an unshown one is actually omitted.
#[test]
fn shown_unknown_effects_never_crowd_out_an_older_unshown_notice() -> Result<(), String> {
    let fixture = Fixture::new("nested-unknown-bound")?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("bound-attempt").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let parent = ToolCallId::new("bound-parent").ok_or("parent")?;
    for index in 0..18 {
        let call = ToolCallId::new(format!("bound-child-{index}")).ok_or("call")?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: call.clone(),
                tool: grapt_types::ToolDefinitionId::new("effect").ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|error| error.to_string())?,
            })
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: ToolTerminalOutcome::Failed {
                    reason: grapt_types::ToolFailureReason::EffectUnknown,
                    detail: None,
                },
            })
            .map_err(|error| error.to_string())?;
    }
    drop(owner);
    let inbox = lock(fixture.inbox()?);
    let mut statement = inbox
        .store()
        .connection()
        .prepare(
            "SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.tool.terminal.1' ORDER BY seq",
        )
        .map_err(|error| error.to_string())?;
    let seqs = statement
        .query_map([fixture.session.as_str()], |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())?
        .map(|seq| {
            seq.map_err(|error| error.to_string())
                .and_then(|seq| u64::try_from(seq).map_err(|error| error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(seqs.len(), 18);
    let texts = |shown: &[u64]| -> Result<Vec<String>, String> {
        let shown = shown.iter().copied().collect();
        Ok(super::super::unknown_effects::notices(
            inbox.store(),
            &fixture.session,
            u64::MAX,
            &shown,
        )?
        .into_iter()
        .filter_map(|(_, item)| match item {
            CodexInputItem::Message(message) => Some(message.content),
            _ => None,
        })
        .collect())
    };
    let marker = "Earlier unknown effects exist and are not listed.";
    // The newest 16 are shown; the two oldest still warn, with no marker.
    let notices = texts(&seqs[2..])?;
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[0].contains("call bound-child-1,"), "{notices:?}");
    assert!(notices[1].contains("call bound-child-0,"), "{notices:?}");
    assert!(!notices.iter().any(|notice| notice.contains(marker)));
    // Nothing shown: the 16 newest warn, and the oldest listed says more exist.
    let notices = texts(&[])?;
    assert_eq!(notices.len(), 16, "{notices:?}");
    assert!(notices[15].contains("call bound-child-2,") && notices[15].contains(marker));
    assert!(!notices[..15].iter().any(|notice| notice.contains(marker)));
    Ok(())
}

#[derive(Debug, Default)]
struct ReconcileIds(std::sync::atomic::AtomicU64);
impl grapt_runtime::application::EventIds for ReconcileIds {
    fn mint(&self) -> grapt_types::EventId {
        let next = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match format!("evt_reconcile_{next}").parse() {
            Ok(id) => id,
            Err(_) => panic!("the fixture event id is valid"),
        }
    }
}
#[derive(Debug)]
struct ReconcileClock;
impl grapt_runtime::application::Clock for ReconcileClock {
    fn now_ms(&self) -> i64 {
        1_000
    }
}

/// SC-REL-01b: reconciliation settles exactly the orphaned nested starts with
/// an unknown effect, never touches a recorded terminal, and is idempotent.
/// An unreadable terminal settles nothing: its call gets one readable unknown
/// effect beside it, and the unreadable row stays, warned about, unused.
#[test]
fn reconcile_settles_only_orphans_once_and_never_replaces_a_terminal() -> Result<(), String> {
    let fixture = Fixture::new("nested-reconcile")?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("reconcile-attempt").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let parent = ToolCallId::new("reconcile-parent").ok_or("parent")?;
    for child in ["lost-child", "settled-child", "unreadable-child"] {
        let call = ToolCallId::new(child).ok_or("call")?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: call.clone(),
                tool: grapt_types::ToolDefinitionId::new("effect").ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|error| error.to_string())?,
            })
            .map_err(|error| error.to_string())?;
        if child != "lost-child" {
            owner
                .commit(DurableFact::ToolTerminal {
                    call,
                    outcome: ToolTerminalOutcome::Completed { output: None },
                })
                .map_err(|error| error.to_string())?;
        }
    }
    drop(owner);
    recent::raw_conn(&fixture)?
        .execute(
            "UPDATE event SET data = '{' WHERE type = 'session.tool.terminal.1'
                  AND json_extract(data, '$.call') = 'unreadable-child'",
            [],
        )
        .map_err(|error| error.to_string())?;
    let mut inbox = lock(fixture.inbox()?);
    assert_eq!(
        require_resolved_effects(inbox.store(), &fixture.session),
        Err(super::super::queries::ORPHANED_REFUSAL.to_owned())
    );
    let terminals = |store: &EventStore| -> Result<Vec<(String, String)>, String> {
        let mut statement = store
            .connection()
            .prepare(
                "SELECT json_extract(data, '$.call'), json_extract(data, '$.outcome.kind')
                 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 AND type = 'session.tool.terminal.1' AND json_valid(data) ORDER BY seq",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([fixture.session.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<_, _>>()
            .map_err(|error| error.to_string())
    };
    for _ in 0..2 {
        let orphans = orphaned_nested(inbox.store(), &fixture.session)?;
        grapt_runtime::reconcile_orphaned_nested(
            inbox.store_mut(),
            &fixture.session,
            &orphans,
            &ReconcileIds::default(),
            &ReconcileClock,
        )?;
    }
    require_resolved_effects(inbox.store(), &fixture.session)?;
    assert_eq!(
        terminals(inbox.store())?,
        [
            ("settled-child".to_owned(), "completed".to_owned()),
            ("lost-child".to_owned(), "failed".to_owned()),
            ("unreadable-child".to_owned(), "failed".to_owned()),
        ]
    );
    let notices = super::super::unknown_effects::notices(
        inbox.store(),
        &fixture.session,
        u64::MAX,
        &std::collections::BTreeSet::new(),
    )?;
    assert_eq!(notices.len(), 3);
    Ok(())
}

/// Review HOLD on 3513d1c9: completed history longer than the pending bound
/// never counts against it; only the unresolved start is returned.
#[test]
fn a_long_completed_nested_history_stays_within_the_orphan_bound() -> Result<(), String> {
    let fixture = Fixture::new("nested-long-history")?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("long-attempt").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let parent = ToolCallId::new("long-parent").ok_or("parent")?;
    let completed = grapt_runtime::MAX_ORPHANED_NESTED + 108;
    for index in 0..=completed {
        let call = ToolCallId::new(format!("long-child-{index}")).ok_or("call")?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: call.clone(),
                tool: grapt_types::ToolDefinitionId::new("effect").ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|error| error.to_string())?,
            })
            .map_err(|error| error.to_string())?;
        // The middle start stays unresolved; every other one completes.
        if index != completed / 2 {
            owner
                .commit(DurableFact::ToolTerminal {
                    call,
                    outcome: ToolTerminalOutcome::Completed { output: None },
                })
                .map_err(|error| error.to_string())?;
        }
    }
    drop(owner);
    let inbox = lock(fixture.inbox()?);
    let orphans = orphaned_nested(inbox.store(), &fixture.session)?;
    assert_eq!(orphans.len(), 1);
    assert_eq!(
        orphans[0].1.as_str(),
        format!("long-child-{}", completed / 2)
    );
    Ok(())
}
