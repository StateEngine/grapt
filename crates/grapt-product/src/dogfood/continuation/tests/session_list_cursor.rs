//! A session-list page that stops among rows it cannot list continues with a
//! compact cursor, whatever those rows' keys hold.
use super::*;

/// Insert `count` unlistable rows that sort ahead of every real Session,
/// each keyed by `key(index)`, through a connection of their own.
fn unlistable_rows(
    fixture: &Fixture,
    count: i64,
    key: impl Fn(i64) -> rusqlite::types::Value,
) -> Result<(), String> {
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    for index in 0..count {
        raw.execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES (?1, 'prj_unlistable', '/work/unlistable', 'unlistable', ?2,
                     'legacy-baseline', 'digest', -1)",
            (key(index), 9_000_000_000_000 + index),
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn body(response: &str) -> Result<serde_json::Value, String> {
    if !response.starts_with("HTTP/1.1 200 ") {
        return Err(format!("the list refused: {response:.400}"));
    }
    let (_, body) = response
        .split_once("\r\n\r\n")
        .ok_or("response has no body")?;
    serde_json::from_str(body).map_err(|error| format!("{error}: {body:.400}"))
}

/// Page `/api/session` two at a time until the fixture's Session is listed.
async fn reaches_the_session(
    label: &str,
    key: fn(i64) -> rusqlite::types::Value,
) -> Result<(), String> {
    let (fixture, app, _driver) = long_session(label)?;
    unlistable_rows(&fixture, 130, key)?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let mut path = "/api/session?limit=2".to_owned();
    let mut found = Err("the list ended before the healthy Session".to_owned());
    for _ in 0..8 {
        let page = match super::read_isolation::get(address, &path)
            .await
            .and_then(|r| body(&r))
        {
            Ok(page) => page,
            Err(error) => {
                found = Err(error);
                break;
            }
        };
        if page["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == fixture.session.as_str()))
        {
            found = Ok(());
            break;
        }
        let Some(next) = page["cursor"]["next"].as_str() else {
            break;
        };
        path = format!("/api/session?limit=2&cursor={next}");
    }
    host.stop().await;
    found
}

#[tokio::test(flavor = "current_thread")]
async fn a_run_of_blob_keys_pages_through_to_the_healthy_session() -> Result<(), String> {
    reaches_the_session("list-cursor-blob", |index| {
        rusqlite::types::Value::Blob(index.to_be_bytes().to_vec())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn a_run_of_oversized_text_keys_pages_through_to_the_healthy_session() -> Result<(), String> {
    reaches_the_session("list-cursor-text", |index| {
        rusqlite::types::Value::Text(format!("{index:03}{}", "k".repeat(9_000)))
    })
    .await
}

/// A continuation whose row now holds another key is refused as the cursor's
/// fault, with the way forward, never as a retryable outage.
#[tokio::test(flavor = "current_thread")]
async fn a_stale_continuation_is_a_bad_cursor_that_says_to_restart() -> Result<(), String> {
    use base64::Engine;
    let (fixture, app, _driver) = long_session("list-cursor-stale")?;
    // More skipped rows than one request reads past, so its cursor names a row.
    unlistable_rows(&fixture, 700, |index| {
        rusqlite::types::Value::Blob(index.to_be_bytes().to_vec())
    })?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let outcome = async {
        let first = body(&super::read_isolation::get(address, "/api/session?limit=2").await?)?;
        let next = first["cursor"]["next"]
            .as_str()
            .ok_or("no continuation")?
            .to_owned();
        let anchor: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&next)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let rowid = anchor["anchor"]["row"].as_i64().ok_or("not a row cursor")?;
        rusqlite::Connection::open(&fixture.database)
            .and_then(|raw| {
                raw.execute(
                    "UPDATE session_list_projection SET session_id = X'FFFF' WHERE rowid = ?1",
                    [rowid],
                )
            })
            .map_err(|error| error.to_string())?;
        super::read_isolation::get(address, &format!("/api/session?limit=2&cursor={next}")).await
    }
    .await;
    host.stop().await;
    let response = outcome?;
    assert!(
        response.starts_with("HTTP/1.1 400 "),
        "a stale cursor was not the cursor's fault: {response:.400}"
    );
    assert!(
        response.contains("list again from the start"),
        "the refusal lost its way forward: {response:.400}"
    );
    Ok(())
}

/// A directory that is not text, or is over the row bound, keeps only its
/// own row out of a directory-scoped list; the list itself still answers.
#[tokio::test(flavor = "current_thread")]
async fn a_directory_scope_answers_past_rows_whose_directory_does_not_read() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("list-directory-unreadable")?;
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    let directory: String = raw
        .query_row(
            "SELECT directory FROM session_list_projection WHERE session_id = ?1",
            [fixture.session.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    for (index, bad) in [
        rusqlite::types::Value::Blob(directory.as_bytes().to_vec()),
        rusqlite::types::Value::Text(format!("{directory}/{}", "x".repeat(300_000))),
        rusqlite::types::Value::Integer(7),
    ]
    .into_iter()
    .enumerate()
    {
        raw.execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES (?1, 'prj_unlistable', ?2, 'unlistable', 9000000000000,
                     'legacy-baseline', 'digest', -1)",
            (format!("ses_bad_directory_{index}"), bad),
        )
        .map_err(|error| error.to_string())?;
    }
    drop(raw);
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("directory", &directory)
        .finish();
    let response = super::read_isolation::get(address, &format!("/api/session?{query}")).await;
    host.stop().await;
    let page = body(&response?)?;
    assert!(
        page["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == fixture.session.as_str())),
        "the scoped list lost the healthy Session: {page}"
    );
    Ok(())
}

/// One request reads on past a page that skipped a full run of rows, so the
/// client is not handed an empty page while healthy Sessions follow.
#[tokio::test(flavor = "current_thread")]
async fn one_request_reads_past_a_page_of_skipped_rows() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("list-server-follows")?;
    unlistable_rows(&fixture, 130, |index| {
        rusqlite::types::Value::Blob(index.to_be_bytes().to_vec())
    })?;
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let response = super::read_isolation::get(address, "/api/session?limit=2").await;
    host.stop().await;
    let page = body(&response?)?;
    assert!(
        page["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == fixture.session.as_str())),
        "the first response stopped at the skipped rows: {page}"
    );
    Ok(())
}

async fn served(
    app: &Arc<crate::dogfood::DogfoodApplication>,
    path: &str,
) -> Result<String, String> {
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        super::read_isolation::service(app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let response = super::read_isolation::get(host.ready().observed_address(), path).await;
    host.stop().await;
    response
}

/// Insert `count` rows the index lists but the wire refuses (a workspace
/// without its wire prefix), newer than the fixture's Session.
fn wire_refused_rows(fixture: &Fixture, count: usize) -> Result<(), String> {
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    for index in 0..count {
        let id = format!("ses_wire_refused_{index}");
        raw.execute(
            "INSERT INTO session_list_legacy_baseline
                (session_id, updated_ms, source_domain, source_digest, cutover_seq)
             VALUES (?1, 9000000000000, 'synthetic-fixture', 'digest', -1)",
            [&id],
        )
        .and_then(|_| {
            raw.execute(
                "INSERT INTO session_list_projection
                    (session_id, project_id, workspace_id, directory, title, updated_ms,
                     provenance_kind, provenance_digest, cutover_seq)
                 VALUES (?1, 'prj_unlistable', 'plain', '/work/unlistable', 'refused',
                         9000000000000, 'legacy-baseline', 'digest', -1)",
                [&id],
            )
        })
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn lists(page: &serde_json::Value, id: &str) -> bool {
    page["data"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["id"] == id))
}

/// Rows the index lists but the wire cannot carry do not end the read: the
/// request goes on to the Session after them.
#[tokio::test(flavor = "current_thread")]
async fn rows_the_wire_refuses_do_not_stop_the_read() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("list-wire-refused")?;
    wire_refused_rows(&fixture, 3)?;
    let page = body(&served(&app, "/api/session?limit=2").await?)?;
    assert!(
        page["data"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == fixture.session.as_str())),
        "the read stopped at rows the wire refused: {page}"
    );
    Ok(())
}

/// A list whose every row to the end is unreadable is unavailable, never an
/// empty page that passes for a store with no Sessions.
#[tokio::test(flavor = "current_thread")]
async fn a_list_of_only_unreadable_rows_is_not_an_empty_one() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("list-only-unreadable")?;
    unlistable_rows(&fixture, 3, |index| {
        rusqlite::types::Value::Blob(index.to_be_bytes().to_vec())
    })?;
    let response = served(&app, "/api/session?search=unlistable").await?;
    assert!(
        response.starts_with("HTTP/1.1 503 "),
        "only unreadable rows passed for none: {response:.400}"
    );
    Ok(())
}

/// Reading back toward newer rows also goes on past rows the wire refuses.
#[tokio::test(flavor = "current_thread")]
async fn a_previous_page_reads_past_rows_the_wire_refuses() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("list-wire-refused-previous")?;
    // Enough that each walk needs every follow the request allows.
    wire_refused_rows(&fixture, 10)?;
    // A Session the wire can carry, newer than every refused row.
    let raw = rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    raw.execute(
        "INSERT INTO session_list_legacy_baseline
            (session_id, updated_ms, source_domain, source_digest, cutover_seq)
         VALUES ('ses_newest', 9500000000000, 'synthetic-fixture', 'digest', -1)",
        [],
    )
    .and_then(|_| {
        raw.execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES ('ses_newest', 'prj_unlistable', '/work/unlistable', 'newest',
                     9500000000000, 'legacy-baseline', 'digest', -1)",
            [],
        )
    })
    .map_err(|error| error.to_string())?;
    drop(raw);
    let first = body(&served(&app, "/api/session?limit=2").await?)?;
    let next = first["cursor"]["next"].as_str().ok_or("no next cursor")?;
    let older = body(&served(&app, &format!("/api/session?limit=2&cursor={next}")).await?)?;
    if !lists(&older, fixture.session.as_str()) {
        return Err(format!("the next page missed the fixture Session: {older}"));
    }
    let previous = older["cursor"]["previous"]
        .as_str()
        .ok_or("no previous cursor")?;
    let newer = body(&served(&app, &format!("/api/session?limit=2&cursor={previous}")).await?)?;
    assert!(
        lists(&newer, "ses_newest"),
        "the previous page stalled among refused rows: {newer}"
    );
    Ok(())
}
