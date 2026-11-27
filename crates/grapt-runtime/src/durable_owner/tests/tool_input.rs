use super::*;
use crate::commit_fault;
use std::sync::atomic::AtomicUsize;

#[derive(Debug, Default)]
struct Commits(AtomicUsize);
impl grapt_store::commit_executor::CommitExecutor for Commits {
    fn execute(&self, work: &mut dyn FnMut()) {
        self.0.fetch_add(1, Ordering::SeqCst);
        work();
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_tool_replay_and_input_commit_atomically_with_identity(
) -> Result<(), Box<dyn std::error::Error>> {
    for refuse in [false, true] {
        let scratch = Scratch::new("native-tool-replay");
        let store = store();
        let clock = FrozenClock;
        let ids = CountingIds::default();
        let sink = CoordinateSink {
            store: Arc::clone(&store),
            notices: Vec::new(),
        };
        let mut owner = DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            sink,
            session(),
            step(1),
            None,
        )?;
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt("native-tool-replay"),
        })?;
        let _fault = if refuse {
            let mut guard = store.lock().map_err(|_| "store lock")?;
            Some(commit_fault::CommitFault::install(
                &mut guard,
                rusqlite::hooks::Action::SQLITE_INSERT,
                "event",
                0,
            )?)
        } else {
            None
        };
        let commits = Arc::new(Commits::default());
        store
            .lock()
            .map_err(|_| "store lock")?
            .install_commit_executor(commits.clone());
        let replay = r#"{"functionCall":{"id":"signed-call","name":"read","args":{}},"thoughtSignature":"synthetic-signature"}"#;
        let input = r#"{"path":"example.txt"}"#;
        let result = owner.commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: ToolCallId::new("signed-call").ok_or("call")?,
                tool: "read".into(),
                input: input.into(),
                execution: grapt_types::ToolExecution::Local,
                state: AdapterState::retain(replay)?,
            }),
        });
        assert_eq!(result.is_err(), refuse);
        let persisted = rows(&store);
        let identities: Vec<_> = persisted
            .iter()
            .filter(|(_, kind, _)| kind.starts_with("session.tool.input.started."))
            .collect();
        let contents: Vec<_> = persisted
            .iter()
            .filter(|(_, kind, _)| kind == "session.content.appended.1")
            .collect();
        if refuse {
            assert!(
                identities.is_empty(),
                "replay refusal must roll identity back"
            );
            assert!(
                contents.is_empty(),
                "replay refusal must roll tool input back"
            );
            assert!(owner.sink.notices.is_empty());
            assert_eq!(owner.next_ordinal, ContentOrdinal::FIRST);
            assert_eq!(
                commits.0.load(Ordering::SeqCst),
                1,
                "the injected commit veto was attempted"
            );
        } else {
            assert_eq!(identities.len(), 1);
            assert_eq!(contents.len(), 2);
            assert_eq!(commits.0.load(Ordering::SeqCst), 1);
            assert_eq!(owner.sink.notices.len(), 2);
            assert_eq!(owner.next_ordinal, ContentOrdinal::new(2));
            for (row, expected, sensitivity) in [
                (contents[0], input, Sensitivity::Ordinary),
                (contents[1], replay, Sensitivity::SecretBearing),
            ] {
                let fact = grapt_event::session_message::decode_appended(&row.2)?;
                let reference = fact.item().payload();
                assert_eq!(
                    owner.artifacts.stat(reference.id())?.sensitivity(),
                    sensitivity
                );
                let budget = grapt_artifact::ReadBudget::new(4096).ok_or("read budget")?;
                assert_eq!(
                    owner.artifacts.fetch(reference, 0, budget)?.bytes(),
                    expected.as_bytes()
                );
            }
            assert_eq!(contents[1].2["item"]["kind"], "opaque");
        }
    }
    Ok(())
}

#[test]
fn tool_identity_and_input_commit_together_or_neither_is_published(
) -> Result<(), Box<dyn std::error::Error>> {
    for execution in [
        grapt_types::ToolExecution::Local,
        grapt_types::ToolExecution::ProviderHosted,
    ] {
        let scratch = Scratch::new(match execution {
            grapt_types::ToolExecution::Local => "tool-segment-local",
            grapt_types::ToolExecution::ProviderHosted => "tool-segment-hosted",
        });
        let store = store();
        let clock = FrozenClock;
        let ids = CountingIds::default();
        let sink = CoordinateSink {
            store: Arc::clone(&store),
            notices: Vec::new(),
        };
        let mut owner = DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            sink,
            session(),
            step(1),
            None,
        )?;
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt("tool-segment"),
        })?;
        let call = ToolCallId::new("segment-call").ok_or("call")?;
        let fact = DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call,
                tool: "shell".into(),
                input: r#"{"command":"echo fixture"}"#.into(),
                execution,
                state: AdapterState::none(),
            }),
        };
        let fault = {
            let mut guard = store.lock().map_err(|_| "store lock poisoned")?;
            commit_fault::CommitFault::install(
                &mut guard,
                rusqlite::hooks::Action::SQLITE_INSERT,
                "event",
                0,
            )?
        };
        assert!(owner.commit(fact.clone()).is_err());
        let refused = rows(&store);
        assert!(!refused
            .iter()
            .any(|(_, kind, _)| kind.starts_with("session.tool.input.started.")));
        assert!(!refused
            .iter()
            .any(|(_, kind, _)| kind == "session.content.appended.1"));
        assert!(
            owner.sink.notices.is_empty(),
            "a failed segment cannot publish content"
        );
        assert_eq!(owner.next_ordinal, ContentOrdinal::FIRST);
        drop(fault);
        let commits = Arc::new(Commits::default());
        store
            .lock()
            .map_err(|_| "store lock poisoned")?
            .install_commit_executor(commits.clone());
        owner.commit(fact)?;
        assert_eq!(
            commits.0.load(Ordering::SeqCst),
            1,
            "one tool input is one intent transaction"
        );
        let committed = rows(&store);
        let identity = committed
            .iter()
            .position(|(_, kind, _)| kind.starts_with("session.tool.input.started."))
            .ok_or("tool identity")?;
        assert_eq!(committed[identity].1, "session.tool.input.started.2");
        let decoded = grapt_event::session_tool_identity::decode(2, &committed[identity].2)?;
        assert_eq!(decoded.execution, execution);
        assert_eq!(decoded.tool.as_str(), "shell");
        assert_eq!(committed[identity + 1].1, "session.content.appended.1");
        assert_eq!(owner.sink.notices.len(), 1);
        // CoordinateSink queries the committed event after the store lock and
        // transaction have both been released.
        assert_eq!(owner.sink.notices[0].1["item"]["ordinal"], 0);
    }
    Ok(())
}

#[test]
fn text_and_replay_remain_atomic_when_the_commit_is_vetoed(
) -> Result<(), Box<dyn std::error::Error>> {
    for refuse in [false, true] {
        let scratch = Scratch::new("text-replay");
        let store = store();
        let clock = FrozenClock;
        let ids = CountingIds::default();
        let mut owner = DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            RecordingSink::default(),
            session(),
            step(1),
            None,
        )?;
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt("text-replay"),
        })?;
        owner.commit(text_opened(0))?;
        owner.commit(text_delta(0, "cited text"))?;
        let _fault = if refuse {
            let mut guard = store.lock().map_err(|_| "store lock")?;
            Some(commit_fault::CommitFault::install(
                &mut guard,
                rusqlite::hooks::Action::SQLITE_INSERT,
                "event",
                0,
            )?)
        } else {
            None
        };
        let commits = Arc::new(Commits::default());
        store
            .lock()
            .map_err(|_| "store lock")?
            .install_commit_executor(commits.clone());
        let result = owner.commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::TextClosed {
                block: BlockOrdinal::new(0),
                state: AdapterState::retain("native cited text replay")?,
            }),
        });
        assert_eq!(result.is_err(), refuse);
        let content: Vec<_> = rows(&store)
            .into_iter()
            .filter(|(_, kind, _)| kind == "session.content.appended.1")
            .collect();
        assert_eq!(content.len(), if refuse { 0 } else { 2 });
        assert_eq!(
            commits.0.load(Ordering::SeqCst),
            1,
            "text and replay share one transaction"
        );
        if !refuse {
            assert!(content
                .iter()
                .any(|(_, _, value)| value["item"]["kind"] == "opaque"));
        }
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn reasoning_and_opaque_replay_close_in_one_atomic_segment(
) -> Result<(), Box<dyn std::error::Error>> {
    for (refuse, visible) in [(false, false), (true, false), (false, true), (true, true)] {
        let scratch = Scratch::new(if refuse {
            "reasoning-segment-refused"
        } else {
            "reasoning-segment"
        });
        let store = store();
        let clock = FrozenClock;
        let ids = CountingIds::default();
        let sink = CoordinateSink {
            store: Arc::clone(&store),
            notices: Vec::new(),
        };
        let mut owner = DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            sink,
            session(),
            step(1),
            None,
        )?;
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt("reasoning-segment"),
        })?;
        if visible {
            owner.commit(text_opened(0))?;
            owner.commit(text_delta(0, "visible prefix"))?;
            owner.commit(text_closed(0))?;
        }
        let contents_before = rows(&store)
            .iter()
            .filter(|(_, kind, _)| kind == "session.content.appended.1")
            .count();
        let ordinal_before = owner.next_ordinal;
        let block = if visible { 1 } else { 0 };
        owner.commit(reasoning_opened(block))?;
        owner.commit(reasoning_delta(block, "reasoning text"))?;
        let notices_before = owner.sink.notices.len();
        let _fault = if refuse {
            let mut guard = store.lock().map_err(|_| "store lock poisoned")?;
            Some(commit_fault::CommitFault::install(
                &mut guard,
                rusqlite::hooks::Action::SQLITE_INSERT,
                "event",
                0,
            )?)
        } else {
            None
        };
        let commits = Arc::new(Commits::default());
        store
            .lock()
            .map_err(|_| "store lock poisoned")?
            .install_commit_executor(commits.clone());
        let result = owner.commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ReasoningClosed {
                block: BlockOrdinal::new(block),
                state: AdapterState::retain("opaque fixture replay")?,
            }),
        });
        let result = if visible {
            result
        } else {
            result?;
            assert_eq!(
                owner.held.len(),
                1,
                "invisible reasoning must remain retryable"
            );
            owner.commit_held()
        };
        assert_eq!(result.is_err(), refuse);
        let contents: Vec<_> = rows(&store)
            .into_iter()
            .filter(|(_, kind, _)| kind == "session.content.appended.1")
            .collect();
        if refuse {
            assert!(
                contents.len() == contents_before,
                "replay refusal must roll prose back too"
            );
            assert_eq!(owner.sink.notices.len(), notices_before);
            assert_eq!(owner.next_ordinal, ordinal_before);
            assert_eq!(
                commits.0.load(Ordering::SeqCst),
                1,
                "the injected commit veto was attempted"
            );
        } else {
            assert_eq!(commits.0.load(Ordering::SeqCst), 1);
            assert_eq!(contents.len(), contents_before + 2);
            let contents = &contents[contents_before..];
            assert_eq!(contents[0].2["item"]["kind"], "reasoning");
            assert_eq!(contents[1].2["item"]["kind"], "opaque");
            assert_eq!(contents[0].2["item"]["ordinal"], ordinal_before.value());
            assert_eq!(contents[1].2["item"]["ordinal"], ordinal_before.value() + 1);
            assert_eq!(owner.sink.notices.len(), notices_before + 2);
        }
    }
    Ok(())
}

#[test]
fn a_blocked_checkpoint_cannot_lose_committed_tool_input_owner_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("tool-reader-segment");
    std::fs::create_dir_all(&scratch.0)?;
    let path = scratch.0.join("store.db");
    let store = Arc::new(Mutex::new(EventStore::open(&path, "tool-reader")?));
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let sink = CoordinateSink {
        store: Arc::clone(&store),
        notices: Vec::new(),
    };
    let mut owner = DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        sink,
        session(),
        step(1),
        None,
    )?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("tool-reader"),
    })?;
    let reader = rusqlite::Connection::open(&path)?;
    reader.execute_batch("BEGIN; SELECT * FROM event")?;
    store
        .lock()
        .map_err(|_| "store lock poisoned")?
        .connection()
        .busy_timeout(std::time::Duration::ZERO)?;
    let call = ToolCallId::new("reader-call").ok_or("call")?;
    owner.commit(DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ToolCalled {
            call,
            tool: "shell".into(),
            input: r#"{"command":"echo reader"}"#.into(),
            execution: grapt_types::ToolExecution::Local,
            state: AdapterState::none(),
        }),
    })?;
    assert_ne!(owner.next_ordinal, ContentOrdinal::FIRST);
    assert_eq!(owner.sink.notices.len(), 1);
    let old: i64 = reader.query_row(
        "SELECT COUNT(*) FROM event WHERE type='session.content.appended.1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(old, 0);
    let committed: Vec<_> = rows(&store)
        .into_iter()
        .filter(|(_, kind, _)| kind == "session.content.appended.1")
        .collect();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].2["item"]["ordinal"], 0);
    reader.execute_batch("COMMIT")?;
    Ok(())
}
