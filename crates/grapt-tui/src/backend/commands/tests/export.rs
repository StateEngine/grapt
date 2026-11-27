use super::*;
use grapt_api::u3a::{
    encode_page, BoundedVec, DtoString, SessionMessageInfoDto, SessionMessagesResponseDto,
    SessionMessagesResponseDtoCursor,
};
use grapt_client::wire;
use grapt_schema::{OptionalNullable, Required};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "grapt-export-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn page(rows: Vec<SessionMessageInfoDto>, next: Option<&str>) -> TestResult<WireResponse> {
    let previous = if rows.is_empty() {
        OptionalNullable::absent()
    } else {
        OptionalNullable::value(DtoString::try_new("previous-offer")?)
    };
    let next = match next {
        Some(next) => OptionalNullable::value(DtoString::try_new(next)?),
        None => OptionalNullable::absent(),
    };
    let dto = SessionMessagesResponseDto::new(
        Required::new(BoundedVec::try_new(rows)?),
        Required::new(SessionMessagesResponseDtoCursor::new(previous, next)),
    );
    Ok(WireResponse {
        status: 200,
        body: encode_page(&dto, wire::limits()?)?,
    })
}
fn history() -> TestResult<Vec<WireResponse>> {
    Ok(vec![
        page(
            vec![wire::user_message("msg_export_first", "**first prompt**")?],
            Some("older-cut"),
        )?,
        page(
            vec![wire::model_switched_message("msg_export_model")?],
            Some("last-cut"),
        )?,
        page(
            vec![wire::assistant_message(
                "msg_export_last",
                &[
                    wire::MessageItem::Text("before tool".into()),
                    wire::MessageItem::Tool(wire::DurableTool {
                        duration_ms: None,
                        id: wire::tool_call_id("call_export")?,
                        name: "shell".into(),
                        progress: vec![],
                        state: wire::DurableToolState::Completed {
                            input: "{}".into(),
                            output: "tool result".into(),
                        },
                    }),
                    wire::MessageItem::Text("after tool".into()),
                ],
                None,
                None,
                None,
            )?],
            Some("terminal-cut"),
        )?,
        page(vec![], None)?,
    ])
}

#[test]
fn export_walks_public_pages_in_order_and_never_overwrites() -> TestResult {
    let directory = Directory::new()?;
    let mut client = prepared(true)?;
    client.enable_paged_history();
    client.browse_history(grapt_client::client::HistoryMove::Beginning)?;
    let selected = client.selected().cloned();
    let reading = client.display_view().cloned();
    for format in ["json", "markdown"] {
        let path = directory.0.join(format!("archive with spaces.{format}"));
        client.peer_mut().export_pages = Some(history()?.into());
        let command = format!("/export {format} \"{}\"", path.display());
        assert!(!submits_prompt(&command));
        let done = run(&mut client, &command)?;
        assert!(done.consume, "{}", done.notice);
        let bytes = std::fs::read(&path)?;
        if format == "json" {
            let json: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                json["sessionID"],
                selected.as_ref().ok_or("selected")?.spelled()
            );
            let rows = json["data"].as_array().ok_or("exported rows")?;
            assert_eq!(
                rows.iter()
                    .filter_map(|row| row["id"].as_str())
                    .collect::<Vec<_>>(),
                ["msg_export_first", "msg_export_model", "msg_export_last"]
            );
        } else {
            let text = std::str::from_utf8(&bytes)?;
            assert!(text.contains("**first prompt**") && text.contains("model-switched"));
            assert!(
                text.find("before tool").ok_or("before")?
                    < text.find("tool result").ok_or("tool")?
            );
            assert!(
                text.find("tool result").ok_or("tool")? < text.find("after tool").ok_or("after")?
            );
        }
        assert!(!run(&mut client, &command)?.consume);
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    assert_eq!(client.selected(), selected.as_ref());
    assert_eq!(client.display_view().cloned(), reading);
    assert!(client.peer_mut().prompts.is_empty());
    assert!(client
        .peer_mut()
        .export_pages
        .as_ref()
        .is_some_and(|pages| pages.is_empty()));
    Ok(())
}

#[test]
fn export_refuses_incomplete_walks_and_publication_failures_without_success_files() -> TestResult {
    let directory = Directory::new()?;
    for case in [
        "cycle",
        "duplicate",
        "empty",
        "refusal",
        "malformed",
        "budget",
        "staging",
    ] {
        let bounds = if case == "budget" {
            Bounds {
                max_message_pages: 1,
                ..Bounds::PRODUCTION
            }
        } else {
            Bounds::PRODUCTION
        };
        let mut client = prepared_with_bounds(true, bounds)?;
        let row = wire::user_message("msg_export_one", "one")?;
        let mut pages = vec![page(vec![row.clone()], Some("cut-a"))?];
        pages.push(match case {
            "cycle" => page(
                vec![wire::user_message("msg_export_two", "two")?],
                Some("cut-a"),
            )?,
            "duplicate" => page(vec![row], None)?,
            "empty" => page(vec![], Some("cut-b"))?,
            "malformed" => WireResponse {
                status: 200,
                body: b"{broken".to_vec(),
            },
            _ => WireResponse {
                status: 503,
                body: Vec::new(),
            },
        });
        let path = directory.0.join(case);
        let stage = PathBuf::from(format!("{}.grapt-export-tmp", path.display()));
        if case == "staging" {
            pages = history()?;
            std::fs::write(&stage, b"existing staging data")?;
        }
        client.peer_mut().export_pages = Some(pages.into());
        let done = run(&mut client, &format!("/export json {}", path.display()))?;
        assert!(
            !done.consume && done.notice.contains("Export refused"),
            "{case}"
        );
        assert!(!path.exists(), "{case}");
        if case == "staging" {
            assert_eq!(std::fs::read(&stage)?, b"existing staging data");
        } else {
            assert!(!stage.exists(), "{case}");
        }
        assert!(client.peer_mut().prompts.is_empty());
    }
    let mut no_session = prepared(false)?;
    assert!(!run(&mut no_session, "/export")?.consume);
    assert!(!run(&mut no_session, "/export yaml")?.consume);
    Ok(())
}
