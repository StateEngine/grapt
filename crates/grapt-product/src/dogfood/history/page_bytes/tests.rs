use super::*;

fn rows() -> Vec<IndexedRow> {
    (0..12)
        .map(|index| {
            let id = format!("msg_byte_{index:02}");
            let text = serde_json::to_string(&"quoted \"line\"\n🦀".repeat(30))
                .unwrap_or_else(|error| panic!("fixture text: {error}"));
            IndexedRow::rendered(
                index,
                id.clone(),
                format!(r#"{{"type":"user","id":"{id}","time":{{"created":0.0}},"text":{text}}}"#),
            )
        })
        .collect()
}
fn render(rows: &[IndexedRow]) -> Result<Vec<String>, String> {
    rows.iter()
        .map(|row| match &row.source {
            RowSource::Rendered(raw) => Ok(raw.clone()),
            _ => Err("fixture source".into()),
        })
        .collect()
}
fn encoded(raw: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let page: grapt_api::u3a::SessionMessagesResponseDto = serde_json::from_str(raw)?;
    Ok(encode_page(&page, U3aLimits::production()?)?)
}
fn page(previous: bool) -> MessagePage {
    MessagePage {
        limit: 200,
        order: Order::Asc,
        cursor: previous.then(|| Cursor {
            id: WireId::parse("msg_anchor".into()).unwrap_or_else(|| panic!("anchor")),
            order: Order::Asc,
            direction: Direction::Previous,
        }),
    }
}

#[test]
fn api01_exact_encoded_framing_cuts_before_the_next_row() -> Result<(), Box<dyn std::error::Error>>
{
    let rows = rows();
    let page = page(false);
    let two = page
        .render(&rows[..2], render)
        .map_err(|e| format!("{e:?}"))?;
    let bound = encoded(&two)?.len();
    let mut visited = 0;
    let raw = page
        .render_bounded(
            &rows,
            |rows| {
                visited += rows.len();
                render(rows)
            },
            bound,
        )
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        encoded(&raw)?.len(),
        bound,
        "the exact bound includes commas and cursor bytes"
    );
    let value: Value = serde_json::from_str(&raw)?;
    assert_eq!(value["data"].as_array().ok_or("data")?.len(), 2);
    assert_eq!(visited, 3, "only the first overflowing row may be rendered");
    let next = MessagePage::parse(super::super::super::decode(
        json!({"cursor":value["cursor"]["next"]}),
    )?)
    .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        next.cursor.ok_or("cursor")?.id.as_inner().as_str(),
        "msg_byte_01"
    );
    Ok(())
}

#[test]
fn api01_previous_byte_pages_keep_the_rows_nearest_the_anchor(
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = rows();
    let page = page(true);
    let tail = &rows[rows.len() - 2..];
    let two = page.render(tail, render).map_err(|e| format!("{e:?}"))?;
    let bound = encoded(&two)?.len();
    let raw = page
        .render_bounded(&rows, render, bound)
        .map_err(|e| format!("{e:?}"))?;
    let value: Value = serde_json::from_str(&raw)?;
    assert_eq!(value["data"][0]["id"], "msg_byte_10");
    assert_eq!(value["data"][1]["id"], "msg_byte_11");
    assert!(encoded(&raw)?.len() <= bound);
    Ok(())
}

#[test]
fn api01_one_row_refusal_names_the_identity_and_the_bound() -> Result<(), Box<dyn std::error::Error>>
{
    let rows = rows();
    let page = page(false);
    let error = page
        .render_bounded(&rows, render, 100)
        .err()
        .ok_or("expected a row refusal")?;
    match error {
        PageRefusal::RowBytes { id, bound } => {
            assert_eq!(id, "msg_byte_00");
            assert_eq!(bound, 100);
        }
        other => return Err(format!("wrong refusal: {other:?}").into()),
    }
    Ok(())
}

/// The live failure's shape: a tool part whose state metadata carries
/// oversized edit diff previews. The whole page still loads, the tool keeps its
/// input and output, and only its metadata is replaced. Re-emitting the row also
/// proves every nested tag ends up first.
#[test]
fn api01_an_oversized_tool_state_metadata_keeps_the_rest_of_the_row(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rows = rows();
    let id = "msg_byte_03";
    let preview = "x".repeat(5_000);
    let oversized = format!(
        r#"{{"type":"assistant","id":"{id}","time":{{"created":0.0}},"agent":"default","model":{{"providerID":"p","id":"m"}},"content":[{{"type":"tool","id":"call_1","name":"edit","time":{{"created":0.0}},"state":{{"status":"completed","input":{{"path":"a.rs"}},"content":[{{"type":"text","text":"applied"}}],"metadata":{{"files":[{{"relativePath":"a.rs","diff":{{"oldText":"{preview}","newText":"{preview}"}}}},{{"relativePath":"b.rs","diff":{{"oldText":"{preview}","newText":"{preview}"}}}}]}}}}}}]}}"#
    );
    rows[3] = IndexedRow::rendered(3, id.to_owned(), oversized.clone());
    assert!(
        serde_json::from_str::<SessionMessageInfoDto>(&oversized).is_err(),
        "the fixture must be one the reader refuses"
    );
    let raw = page(false)
        .render(&rows, render)
        .map_err(|error| format!("{error:?}"))?;
    let value: Value = serde_json::from_str(&raw)?;
    let data = value["data"].as_array().ok_or("data")?;
    assert_eq!(data.len(), rows.len(), "every row of the page is kept");
    let row = data.iter().find(|row| row["id"] == id).ok_or("the row")?;
    let tool = &row["content"][0];
    assert_eq!(tool["name"], "edit", "the tool keeps its identity");
    assert_eq!(tool["state"]["input"]["path"], "a.rs", "input is untouched");
    assert_eq!(tool["state"]["content"][0]["text"], "applied");
    assert!(tool["state"]["metadata"].get("files").is_none());
    assert!(
        tool["state"]["metadata"]["metadataDropped"].is_object(),
        "the dropped metadata is marked in one vocabulary"
    );
    typed_page_matches_legacy(&page(false), &rows)?;
    Ok(())
}

/// A user's own nested key named `metadata` inside a tool input is decoded
/// under the larger tool-input budget, so it is never treated as this reader's
/// metadata and never replaced.
#[test]
fn api01_a_user_key_named_metadata_inside_a_tool_input_is_kept(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rows = rows();
    let id = "msg_byte_04";
    // Over the metadata budget's scalar bound but inside the larger tool-input
    // budget, so the retry must leave it alone rather than mistake it for the
    // state's own metadata.
    let note = "u".repeat(10_000);
    let preview = "x".repeat(5_000);
    let oversized = format!(
        r#"{{"type":"assistant","id":"{id}","time":{{"created":0.0}},"agent":"default","model":{{"providerID":"p","id":"m"}},"content":[{{"type":"tool","id":"call_2","name":"write","time":{{"created":0.0}},"state":{{"status":"completed","input":{{"path":"a.json","metadata":{{"note":"{note}"}}}},"content":[],"metadata":{{"files":[{{"relativePath":"a.rs","diff":{{"oldText":"{preview}","newText":"{preview}"}}}}]}}}}}}]}}"#
    );
    rows[4] = IndexedRow::rendered(4, id.to_owned(), oversized.clone());
    assert!(serde_json::from_str::<SessionMessageInfoDto>(&oversized).is_err());
    let raw = page(false)
        .render(&rows, render)
        .map_err(|error| format!("{error:?}"))?;
    let value: Value = serde_json::from_str(&raw)?;
    let data = value["data"].as_array().ok_or("data")?;
    let row = data.iter().find(|row| row["id"] == id).ok_or("the row")?;
    assert_eq!(
        row["content"][0]["state"]["input"]["metadata"]["note"], note,
        "the user's own nested key survives"
    );
    assert!(row["content"][0]["state"]["metadata"]
        .get("files")
        .is_none());
    Ok(())
}

fn typed_page_matches_legacy(
    page: &MessagePage,
    rows: &[IndexedRow],
) -> Result<Value, Box<dyn std::error::Error>> {
    let raw = page
        .render(rows, render)
        .map_err(|error| format!("{error:?}"))?;
    let legacy: Value = serde_json::from_str(&raw)?;
    let (data, cursor) = page
        .render_typed(rows, render)
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(serde_json::to_value(data)?, legacy["data"]);
    assert_eq!(serde_json::to_value(cursor)?, legacy["cursor"]);
    assert!(encoded(&raw)?.len() <= U3A_PRODUCTION_LIMITS.page_bytes);
    Ok(legacy)
}

#[test]
fn typed_history_keeps_the_legacy_byte_cut_and_cursor_in_both_directions(
) -> Result<(), Box<dyn std::error::Error>> {
    let text = serde_json::to_string(&"quoted \"line\"\n🦀".repeat(8_000))?;
    let rows: Vec<_> = (0..80)
        .map(|index| {
            let id = format!("msg_large_{index:02}");
            IndexedRow::rendered(
                index,
                id.clone(),
                format!(r#"{{"type":"user","id":"{id}","time":{{"created":0.0}},"text":{text}}}"#),
            )
        })
        .collect();
    for previous in [false, true] {
        let page = page(previous);
        let value = typed_page_matches_legacy(&page, &rows)?;
        let data = value["data"].as_array().ok_or("data")?;
        assert!(
            !data.is_empty() && data.len() < rows.len(),
            "the production byte bound cuts this page"
        );
        if previous {
            assert_eq!(data.last().ok_or("last row")?["id"], "msg_large_79");
        } else {
            assert_eq!(data.first().ok_or("first row")?["id"], "msg_large_00");
        }
        let empty = typed_page_matches_legacy(&page, &[])?;
        assert_eq!(empty["data"].as_array().ok_or("empty data")?.len(), 0);
        assert_eq!(empty["cursor"].as_object().ok_or("empty cursor")?.len(), 0);
    }
    Ok(())
}

#[test]
fn review_foreign_rendered_identity_refuses_the_page_by_name(
) -> Result<(), Box<dyn std::error::Error>> {
    // A renderer invariant violation is a bug, not unreadable stored data: it
    // keeps the by-name page refusal and is never served as a marker row.
    let mut rows = rows();
    rows[3] = IndexedRow::rendered(
        3,
        "msg_byte_03".into(),
        r#"{"type":"user","id":"msg_foreign","time":{"created":0.0},"text":"foreign content"}"#
            .into(),
    );
    for previous in [false, true] {
        let page = page(previous);
        for refusal in [
            page.render(&rows, render).err(),
            page.render_typed(&rows, render).err(),
        ] {
            match refusal.ok_or("a foreign identity was served")? {
                PageRefusal::Failed(error) => {
                    assert!(error.contains("msg_byte_03"), "{error}");
                    assert!(error.contains("rendered under another id"), "{error}");
                    assert!(!error.contains("foreign content"), "{error}");
                }
                other => return Err(format!("wrong refusal: {other:?}").into()),
            }
        }
    }
    Ok(())
}

#[test]
fn review_an_undecodable_stored_item_keeps_its_row_and_neighbours(
) -> Result<(), Box<dyn std::error::Error>> {
    // Stored data that cannot decode (an explicit null where the DTO forbids
    // it) degrades only that item, under its own id and seq.
    let mut rows = rows();
    rows[3] = IndexedRow::rendered(
        3,
        "msg_byte_03".into(),
        r#"{"type":"user","id":"msg_byte_03","time":{"created":0.0},"text":"stored","metadata":null}"#
            .into(),
    );
    for previous in [false, true] {
        let value = typed_page_matches_legacy(&page(previous), &rows)?;
        let data = value["data"].as_array().ok_or("data")?;
        assert_eq!(data.len(), rows.len());
        let marker = data
            .iter()
            .find(|row| row["id"] == "msg_byte_03")
            .ok_or("selected identity")?;
        assert_eq!(marker["type"], "system");
        assert_eq!(marker["metadata"]["unreadableMessage"], true);
        assert_eq!(marker["metadata"]["seq"], 3);
        for neighbour in ["msg_byte_02", "msg_byte_04"] {
            assert!(data
                .iter()
                .any(|row| row["id"] == neighbour && row["type"] == "user"));
        }
    }
    Ok(())
}

#[test]
fn review_a_render_failure_refuses_only_the_pages_holding_the_row(
) -> Result<(), Box<dyn std::error::Error>> {
    // A render failure (a lost artifact, a transient read error) says nothing
    // about the stored item, so it must not become a permanent-looking marker.
    fn fail_one(rows: &[IndexedRow]) -> Result<Vec<String>, String> {
        if rows.iter().any(|row| row.id == "msg_byte_03") {
            Err("the selected content cannot be read".into())
        } else {
            render(rows)
        }
    }
    let rows = rows();
    let page = page(false);
    for refusal in [
        page.render(&rows, fail_one).err(),
        page.render_typed(&rows, fail_one).err(),
    ] {
        match refusal.ok_or("a render failure was served")? {
            PageRefusal::Failed(error) => {
                assert_eq!(error, "the selected content cannot be read");
            }
            other => return Err(format!("wrong refusal: {other:?}").into()),
        }
    }
    let spared: Vec<_> = rows
        .into_iter()
        .filter(|row| row.id != "msg_byte_03")
        .collect();
    let raw = page
        .render(&spared, fail_one)
        .map_err(|error| format!("{error:?}"))?;
    assert!(!raw.contains("unreadableMessage"));
    assert!(encoded(&raw)?.len() <= U3A_PRODUCTION_LIMITS.page_bytes);
    Ok(())
}
