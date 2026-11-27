//! Metadata faults remain local through authenticated HTTP and CLI consumers.
use super::*;
use serde_json::Value;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn with_sessions<T>(
    label: &str,
    run: impl FnOnce(&Path, SocketAddr, &[String]) -> TestResult<T>,
) -> TestResult<T> {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 150_000);
    let provider = ProviderFixture::start(vec![Script::SuccessText("healthy resume completed")]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let sessions = (0..3).map(|_| create(address)).collect::<Vec<_>>();
    let result = run(&root, address, &sessions);
    child.stop_after_witness(&root);
    drop(child);
    drop(provider);
    process_fixture::cleanup(&root);
    result
}

fn corrupt_seed(root: &Path, session: &str) -> TestResult {
    let database = rusqlite::Connection::open(root.join("database.sqlite3"))?;
    let changed = database.execute(
        "UPDATE event SET data=json_remove(data,'$.info.directory')
         WHERE aggregate_id=?1 AND type='session.created.1'",
        [session],
    )?;
    if changed != 1 {
        return Err(format!("fixture changed {changed} seeds instead of one").into());
    }
    Ok(())
}

fn response(address: SocketAddr, path: &str) -> TestResult<(u16, Value)> {
    let (status, body) = request(address, "GET", path, "");
    Ok((status, serde_json::from_slice(&body)?))
}

fn two_pages(address: SocketAddr, order: &str) -> TestResult<Vec<(u16, Value)>> {
    let first = response(address, &format!("/api/session?order={order}&limit=2"))?;
    let Some(cursor) = first.1["cursor"]["next"].as_str() else {
        return Ok(vec![first]);
    };
    let second = response(address, &format!("/api/session?cursor={cursor}&limit=2"))?;
    Ok(vec![first, second])
}

#[test]
fn corrupt_metadata_keeps_both_page_orders_cursors_and_client_titles() -> TestResult {
    let (before, after, corrupt) = with_sessions("metadata-page-isolation", |root, address, _| {
        let before = [two_pages(address, "asc")?, two_pages(address, "desc")?];
        let corrupt = before[0][0].1["data"][1]["id"]
            .as_str()
            .ok_or("middle session absent")?
            .to_owned();
        corrupt_seed(root, &corrupt)?;
        Ok((
            before,
            [two_pages(address, "asc")?, two_pages(address, "desc")?],
            corrupt,
        ))
    })?;
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(before.len(), 2);
        assert_eq!(after.len(), 2, "corruption removed the page cursor");
        for ((_, before), (status, after)) in before.iter().zip(after) {
            assert_eq!(*status, 200, "one bad row refused its neighbors: {after}");
            assert_eq!(before["cursor"], after["cursor"]);
            let expected = before["data"].as_array().ok_or("baseline rows")?;
            let actual = after["data"].as_array().ok_or("isolated rows")?;
            assert_eq!(expected.len(), actual.len());
            let clients = grapt_client::wire::decode_sessions(&serde_json::to_vec(after)?)?;
            for ((expected, actual), client) in expected.iter().zip(actual).zip(clients) {
                for field in ["id", "projectID", "parentID", "title", "location"] {
                    assert_eq!(expected[field], actual[field], "changed {field}");
                }
                assert_eq!(expected["time"]["updated"], actual["time"]["updated"]);
                assert_eq!(client.title, actual["title"].as_str().ok_or("title")?);
                if actual["id"] == corrupt {
                    assert_eq!(actual["metadataStatus"], "quarantined");
                    assert!(actual["time"].get("created").is_none());
                    assert!(actual.get("cost").is_none());
                    assert!(actual.get("tokens").is_none());
                    assert!(client.detail.created_ms.is_none());
                    assert!(client.detail.context_tokens.is_none());
                } else {
                    assert_eq!(expected, actual, "a healthy row changed");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn selected_metadata_distinguishes_missing_corrupt_and_storage_failure() -> TestResult {
    let observed = with_sessions("metadata-selected-isolation", |root, address, sessions| {
        let route = format!("/api/session/{}", sessions[0]);
        let healthy = response(address, &route)?;
        let missing = response(address, "/api/session/ses_missing_metadata")?;
        corrupt_seed(root, &sessions[0])?;
        let corrupt = response(address, &route)?;
        let database = rusqlite::Connection::open(root.join("database.sqlite3"))?;
        database.execute_batch("ALTER TABLE event RENAME TO metadata_unavailable_event")?;
        let storage = response(address, &format!("/api/session/{}", sessions[1]))?;
        let unknown_during_failure = response(address, "/api/session/ses_missing_metadata")?;
        database.execute_batch("ALTER TABLE metadata_unavailable_event RENAME TO event")?;
        Ok([healthy, missing, corrupt, storage, unknown_during_failure])
    })?;
    assert_eq!(observed[0].0, 200);
    assert_eq!(observed[1].0, 404);
    for (status, body) in &observed[2..] {
        assert_eq!(*status, 503, "storage refusal was misclassified: {body}");
        assert!(body.to_string().contains("ServiceUnavailableError"));
    }
    Ok(())
}

#[test]
fn cli_resume_uses_a_healthy_neighbor_and_never_creates_for_unreadable_rows() -> TestResult {
    let (mixed, all, sessions_before, sessions_after) =
        with_sessions("metadata-cli-resume", |root, address, sessions| {
            let listed = json(address, "GET", "/api/session?parentID=null&order=desc", "");
            let newest = listed["data"][0]["id"].as_str().ok_or("newest session")?;
            corrupt_seed(root, newest)?;
            let service = root.to_str().ok_or("UTF-8 service path")?;
            let arguments = [
                "run",
                "--service",
                service,
                "--continue",
                "--prompt",
                "go",
                "--jsonl",
            ];
            let mixed = cli_continuity::cli(root, &arguments);
            for session in sessions {
                if session != newest {
                    corrupt_seed(root, session)?;
                }
            }
            let before = response(address, "/api/session?limit=128")?;
            let all = cli_continuity::cli(root, &arguments);
            let after = response(address, "/api/session?limit=128")?;
            Ok((mixed, all, before, after))
        })?;
    assert_eq!(mixed.code, Some(0), "{}", mixed.stderr);
    assert!(mixed.stdout.contains("healthy resume completed"));
    assert_ne!(all.code, Some(0));
    assert!(all.stderr.contains("unreadable metadata"), "{}", all.stderr);
    assert_eq!((sessions_before.0, sessions_after.0), (200, 200));
    assert_eq!(sessions_before.1["data"], sessions_after.1["data"]);
    assert_eq!(
        sessions_after.1["data"]
            .as_array()
            .ok_or("session rows")?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn cli_resume_reaches_a_healthy_session_after_an_unreadable_first_page() -> TestResult {
    let (run, healthy) = with_sessions("metadata-resume-pages", |root, address, _| {
        // More sessions than the ordinary resume page; only the oldest is usable.
        for _ in 0..48 {
            create(address);
        }
        let oldest = json(address, "GET", "/api/session?order=asc&limit=1", "");
        let healthy = oldest["data"][0]["id"]
            .as_str()
            .ok_or("oldest session")?
            .to_owned();
        let database = rusqlite::Connection::open(root.join("database.sqlite3"))?;
        let changed = database.execute(
            "UPDATE event SET data=json_remove(data,'$.info.directory')
             WHERE type='session.created.1' AND aggregate_id<>?1",
            [&healthy],
        )?;
        if changed != 50 {
            return Err(format!("fixture changed {changed} unreadable seeds").into());
        }
        let service = root.to_str().ok_or("UTF-8 service path")?;
        let run = cli_continuity::cli(
            root,
            &[
                "run",
                "--service",
                service,
                "--continue",
                "--prompt",
                "go",
                "--jsonl",
            ],
        );
        Ok((run, healthy))
    })?;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(run.stdout.contains("healthy resume completed"));
    assert_eq!(run.receipt()["session"], healthy);
    Ok(())
}
