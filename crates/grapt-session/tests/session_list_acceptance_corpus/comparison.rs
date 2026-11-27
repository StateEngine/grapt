fn page_ids(store: &mut EventStore, size: usize) -> Fallible<Vec<String>> {
    let criteria = SessionListCriteria::new(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        size,
        SessionListLimits::production(),
    )?;
    Ok(SessionListStore::over(store)
        .list(&criteria)?
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str().to_owned())
        .collect())
}

fn final_projection(store: &EventStore) -> Fallible<Vec<Value>> {
    let conn = store.connection();
    let mut statement = conn.prepare(
        "SELECT session_id, updated_ms, title, provenance_kind, cutover_seq
         FROM session_list_projection
         ORDER BY updated_ms ASC, session_id COLLATE BINARY ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(json!({
                "session_id": row.get::<_, String>(0)?,
                "updated_ms": row.get::<_, i64>(1)?,
                "title": row.get::<_, String>(2)?,
                "provenance_kind": row.get::<_, String>(3)?,
                "cutover_seq": row.get::<_, i64>(4)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[test]
fn every_action_executes_against_the_implementation_and_matches_the_declared_outputs() -> TestResult
{
    let root = corpus_root();
    let actions = read_jsonl(&root.join("actions.jsonl"))?;
    assert!(!actions.is_empty(), "the corpus must declare actions");
    for (index, action) in actions.iter().enumerate() {
        assert_eq!(
            action["ordinal"].as_i64(),
            Some(i64::try_from(index + 1)?),
            "actions must be ordinal-ordered from one"
        );
    }
    let initial = read_json(&root.join("initial-state.json"))?;

    let run = execute(&actions, &initial)?;

    // Every declared expectation is compared against what the implementation
    // actually produced. A surface the run did not produce is a failure, not
    // an omission.
    let expected_api = read_jsonl(&root.join("expected/api.jsonl"))?;
    let produced_api = &run.api;
    assert_eq!(
        produced_api.len(),
        expected_api.len(),
        "api surface: produced {:#?}\nexpected {:#?}",
        produced_api,
        expected_api
    );
    for (produced, expected) in produced_api.iter().zip(expected_api.iter()) {
        assert_eq!(produced["action"], expected["action"], "api action ordinal");
        assert_eq!(
            produced["outcome"], expected["outcome"],
            "api outcome for action {}",
            expected["action"]
        );
        if expected["outcome"] == "page" {
            assert_eq!(
                produced["ids"], expected["ids"],
                "page ids for action {}",
                expected["action"]
            );
            assert_eq!(produced["order"], expected["order"]);
        } else {
            assert_eq!(
                produced["error"], expected["error"],
                "refusal for action {}",
                expected["action"]
            );
        }
    }

    let expected_durable = read_jsonl(&root.join("expected/durable.jsonl"))?;
    for (produced, expected) in run.durable.iter().zip(expected_durable.iter()) {
        assert_eq!(produced["action"], expected["action"]);
        assert_eq!(produced["aggregate"], expected["aggregate"]);
        assert_eq!(produced["seq"], expected["seq"], "durable sequence");
        assert_eq!(produced["type"], expected["type"], "durable type");
    }
    assert_eq!(
        run.durable.len(),
        expected_durable.len(),
        "durable surface length"
    );

    let expected_events = read_jsonl(&root.join("expected/events.jsonl"))?;
    assert_eq!(run.events, expected_events, "post-commit notice surface");

    let expected_projection = read_json(&root.join("expected/projection.json"))?;
    let produced_rows = final_projection(&run.store)?;
    assert_eq!(
        Value::Array(produced_rows),
        expected_projection["rows"],
        "final canonical projection"
    );
    assert_eq!(
        expected_projection["cross_page_snapshot_promised"],
        json!(false)
    );

    // Waivers are a meaningful assertion, not decoration.
    let waivers = read_json(&root.join("expected/waivers.json"))?;
    assert!(
        waivers["waivers"]
            .as_array()
            .is_some_and(|list| !list.is_empty()),
        "the waiver file must state every intentional non-parity"
    );

    let _ = std::fs::remove_dir_all(&run.root);
    let _ = &run.db;
    Ok(())
}
