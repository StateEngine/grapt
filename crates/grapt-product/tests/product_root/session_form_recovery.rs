//! Ordinary Product routes recover pending Session forms under profile ownership.
use super::*;
use grapt_types::IdAuthority;
use restart_process::{Stage, StartupTrace};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Profile(PathBuf);
impl Profile {
    fn new() -> TestResult<Self> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("product-form-{}", ids.mint().as_str()));
        fs::create_dir(&root)?;
        let profile = Self(root);
        let database = profile.0.join("database.sqlite3");
        drop(EventStore::open(&database, "session-form-fixture")?);
        let migrations = ProductionMigrations::load()?;
        apply_migrations(&database, &profile.0, "0.1.0", &migrations.as_slice())?;
        write_inputs(&profile.0);
        fs::create_dir(profile.0.join("home"))?;
        fs::create_dir(profile.0.join("config"))?;
        Ok(profile)
    }

    fn command(&self) -> Command {
        let mut command =
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
        command
            .args(["serve-terminal", "--service"])
            .arg(&self.0)
            .args(["--port", "0"])
            .env_remove("GRAPT_CONFIG")
            .env_remove("SECODE_CONFIG") // grapt-rename: keep
            .env_remove("GRAPT_CONFIG_CONTENT")
            .env_remove("SECODE_CONFIG_CONTENT") // grapt-rename: keep
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("GRAPT_TEST_HOME", self.0.join("home"))
            .env("GRAPT_CONFIG_DIR", self.0.join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn launch(&self) -> TestResult<RestartProcess> {
        Ok(RestartProcess(self.command().spawn()?))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn clean_stop(process: &mut RestartProcess) {
    drop(process.0.stdin.take());
    assert!(process.exit().success(), "clean Product shutdown failed");
}

fn pending_form(ready: &Value, root: &Path) -> TestResult<(String, String)> {
    let response = restart_request(
        ready,
        "POST",
        "/api/session",
        &json!({
            "location":{"directory":root.to_str().ok_or("path encoding")?},
            "model":{"providerID":"openai", "id":"gpt-5.6"}
        })
        .to_string(),
    );
    let session = response["data"]["id"]
        .as_str()
        .ok_or("session id absent")?
        .to_owned();
    let path = format!("/api/session/{session}/form/frm_restart");
    restart_request(
        ready,
        "POST",
        &format!("/api/session/{session}/form"),
        &json!({"id":"frm_restart", "title":"Branch?", "fields":[
            {"key":"branch", "type":"string", "required":true}]})
        .to_string(),
    );
    Ok((session, path))
}

fn facts(root: &Path, session: &str) -> TestResult<Vec<String>> {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = connection.prepare(
        "SELECT type FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type LIKE 'form.%' ORDER BY seq")?;
    let rows = statement
        .query_map([session], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn restart_journey(trace: &mut StartupTrace, crash: bool, cancel_first: bool) -> TestResult {
    let profile = Profile::new()?;
    let stages = if cancel_first {
        [
            Stage::CancelInitial,
            Stage::CancelRecovered,
            Stage::CancelFinal,
        ]
    } else {
        [
            Stage::AnswerInitial,
            Stage::AnswerRecovered,
            Stage::AnswerFinal,
        ]
    };
    let (mut first, ready) = trace.launch(stages[0], &profile.0, profile.command());
    let (session, path) = pending_form(&ready, &profile.0)?;
    if crash {
        first.kill_and_reap();
    } else {
        first.clean_stop();
    }
    let (mut recovered, ready) = trace.launch(stages[1], &profile.0, profile.command());
    let pending = restart_request(&ready, "GET", &format!("{path}/state"), "");
    assert_eq!(pending["data"]["status"], "pending");
    // A malformed answer must remain correctable after restart.
    assert_eq!(
        restart_response(
            &ready,
            "POST",
            &format!("{path}/reply"),
            r#"{"answer":{"branch":42}}"#
        )
        .0,
        400
    );
    let (operation, body, terminal, fact) = if cancel_first {
        ("cancel", "{}", "cancelled", "form.cancelled.1")
    } else {
        (
            "reply",
            r#"{"answer":{"branch":"main"}}"#,
            "answered",
            "form.replied.1",
        )
    };
    assert_eq!(
        restart_response(&ready, "POST", &format!("{path}/{operation}"), body).0,
        204,
        "first recovered settlement must succeed"
    );
    let state = restart_request(&ready, "GET", &format!("{path}/state"), "");
    assert_eq!(state["data"]["status"], terminal);
    recovered.clean_stop();
    let (mut final_process, ready) = trace.launch(stages[2], &profile.0, profile.command());
    for (operation, body) in [
        ("reply", r#"{"answer":{"branch":"again"}}"#),
        ("cancel", "{}"),
    ] {
        assert_eq!(
            restart_response(&ready, "POST", &format!("{path}/{operation}"), body).0,
            409,
            "restart must not replay settlement"
        );
    }
    final_process.clean_stop();
    assert_eq!(facts(&profile.0, &session)?, ["form.created.1", fact]);
    Ok(())
}

#[test]
fn clean_restart_recovers_answer_and_cancel_once() -> TestResult {
    let mut trace = StartupTrace::new("clean restart");
    restart_journey(&mut trace, false, false)?;
    restart_journey(&mut trace, false, true)
}

#[test]
fn killed_product_recovers_answer_and_cancel_once() -> TestResult {
    let mut trace = StartupTrace::new("killed restart");
    restart_journey(&mut trace, true, false)?;
    restart_journey(&mut trace, true, true)
}

fn forced_readiness_failure(completed: &[Stage], stage: Stage) -> TestResult<String> {
    let profile = Profile::new()?;
    let mut trace = StartupTrace::new("forced restart");
    for &stage in completed {
        let (mut process, _) = trace.launch(stage, &profile.0, profile.command());
        process.clean_stop();
    }
    let mut command = profile.command();
    command.arg("--invalid-startup-witness");
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        trace.launch(stage, &profile.0, command)
    }))
    .err()
    .ok_or("invalid product command unexpectedly became ready")?;
    let message = failed
        .downcast_ref::<String>()
        .ok_or("panic was not text")?
        .clone();
    // The disposable profile is gone before the retained diagnostic is read.
    drop(profile);
    assert!(message.contains("stdout retained tail"), "{message}");
    assert!(message.contains("grapt.product.launch.v1"), "{message}");
    assert!(message.contains("stderr retained tail"), "{message}");
    assert!(message.contains("product launch refused"), "{message}");
    assert!(message.contains("cap=4096B"), "{message}");
    Ok(message)
}

#[test]
fn initial_readiness_failure_retains_its_stage_and_output() -> TestResult {
    let message = forced_readiness_failure(&[], Stage::AnswerInitial)?;
    assert!(message.contains("stage=answer/initial"), "{message}");
    assert!(message.contains("completed stages=[]"), "{message}");
    Ok(())
}

#[test]
fn final_readiness_failure_retains_all_completed_stage_timings() -> TestResult {
    let message = forced_readiness_failure(
        &[
            Stage::AnswerInitial,
            Stage::AnswerRecovered,
            Stage::AnswerFinal,
            Stage::CancelInitial,
            Stage::CancelRecovered,
        ],
        Stage::CancelFinal,
    )?;
    assert!(message.contains("stage=cancel/final"), "{message}");
    for stage in [
        "answer/initial",
        "answer/recovered",
        "answer/final",
        "cancel/initial",
        "cancel/recovered",
    ] {
        assert!(message.contains(&format!("{stage}=")), "{message}");
    }
    assert!(message.contains("ms; completed stages="), "{message}");
    Ok(())
}

#[test]
fn a_second_product_cannot_take_a_live_profiles_session_writer() -> TestResult {
    let profile = Profile::new()?;
    let mut owner = profile.launch()?;
    let ready = owner.ready(&profile.0);
    let (session, path) = pending_form(&ready, &profile.0)?;
    let mut contender = profile.launch()?;
    assert!(
        !contender.exit().success(),
        "live profile ownership must refuse replacement"
    );
    assert_eq!(
        restart_response(
            &ready,
            "POST",
            &format!("{path}/reply"),
            r#"{"answer":{"branch":"main"}}"#
        )
        .0,
        204,
        "live owner must retain its Session fence"
    );
    clean_stop(&mut owner);
    assert_eq!(
        facts(&profile.0, &session)?,
        ["form.created.1", "form.replied.1"]
    );
    Ok(())
}
