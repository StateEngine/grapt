//! CLI-COMPATIBILITY-MATRIX-01 witnesses: resume, fork and project-scoped session selection.
//! Split out of `compatibility.rs` by HYGIENE-FOLLOWUPS-01; no test was renamed or dropped.

use super::*;

/// A Session record as `POST /api/session` and `POST /api/session/{id}/fork`
/// return it.
fn session_record(id: &str, parent: Option<&str>) -> String {
    session_record_in(id, parent, "prj_a", "/w")
}

fn session_record_in(id: &str, parent: Option<&str>, project: &str, directory: &str) -> String {
    let parent = parent.map_or(String::new(), |parent| format!(r#""parentID":"{parent}","#));
    format!(
        r#"{{"data":{{"id":"{id}",{parent}"projectID":"{project}","cost":0.0,"tokens":{{"input":0.0,"output":0.0,"reasoning":0.0,"cache":{{"read":0.0,"write":0.0}}}},"time":{{"created":1.0,"updated":1.0}},"title":"t","location":{{"directory":"{directory}"}}}}}}"#
    )
}

/// The requests of one settled text run of "go" in `session`.
fn settled_run(session: &str) -> Vec<Script> {
    let accepted = PROMPT_ACCEPTED.replace("ses_a", session);
    let user = r#"{"type":"user","id":"msg_user2","time":{"created":3.0},"text":"go"}"#.to_owned();
    let done = page(&[assistant("msg_new", "forked answer", "", true), user]);
    vec![
        Script::sse("GET", "/api/event", ""),
        Script::json(
            "POST",
            format!("/api/session/{session}/prompt"),
            200,
            &accepted,
        ),
        Script::json("GET", format!("/api/session/{session}/message"), 200, &done),
        Script::json(
            "GET",
            format!("/api/session/{session}/permission"),
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
        Script::json("GET", format!("/api/session/{session}/message"), 200, &done),
    ]
}

async fn run_with(
    fork: bool,
    selector: SessionSelector,
    scripts: Vec<Script>,
) -> TestResult<(grapt_cli::RunReceipt, Vec<String>)> {
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let receipt = Runner::new(client(&endpoint)?)
        .with_fork(fork)
        .run_prompt(selector, "go".to_owned(), &mut output)
        .await?;
    Ok((receipt, server.await??))
}

/// `run --fork` as the TypeScript `run` did (session-target.ts): the selected
/// Session is forked first and the prompt goes to the CHILD; the parent receives
/// nothing. A fork needs a Session to fork (run.ts), so `--fork` alone or with
/// `--new` is refused, and `attach` does not take it.
#[tokio::test]
async fn run_fork_forks_the_selected_session_and_prompts_the_child() -> TestResult {
    for flag in [&["-s", "ses_a"][..], &["-c"][..]] {
        let arguments = [&["run", "--service", "r"][..], flag, &["--fork", "go"][..]].concat();
        assert!(
            matches!(parse(&arguments)?, CliCommand::Run { fork: true, .. }),
            "{arguments:?}"
        );
    }
    for arguments in [
        &["run", "--service", "r", "--fork", "go"][..],
        &["run", "--service", "r", "--new", "--fork", "go"][..],
    ] {
        let refused = parse(arguments);
        assert_eq!(refused, Err(CommandError::ForkTarget), "{arguments:?}");
        assert_eq!(
            CommandError::ForkTarget.to_string(),
            "--fork requires --continue or --session"
        );
    }
    assert!(parse(&["attach", "--service", "r", "-s", "ses_a", "--fork"]).is_err());

    let mut scripts = vec![Script::json(
        "POST",
        "/api/session/ses_a/fork",
        200,
        &session_record("ses_b", Some("ses_a")),
    )];
    scripts.extend(settled_run("ses_b"));
    let selector = SessionSelector::Id {
        session: "ses_a".parse()?,
        project: None,
    };
    let (receipt, requests) = run_with(true, selector, scripts).await?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert_eq!(
        receipt.session.to_string(),
        "ses_b",
        "CCM116 the run did not continue in the fork"
    );
    assert_eq!(
        requests.first().map(String::as_str),
        Some("POST /api/session/ses_a/fork")
    );
    let parent = requests
        .iter()
        .filter(|request| request.contains("/ses_a/"))
        .collect::<Vec<_>>();
    assert_eq!(
        parent.len(),
        1,
        "the parent received more than its fork: {requests:?}"
    );
    Ok(())
}

/// A fork the server refuses ends the run with its own named outcome, before
/// any prompt.
#[tokio::test]
async fn a_refused_fork_is_named_and_prompts_nothing() -> TestResult {
    let (endpoint, server) =
        spawn_server(vec![Script::status("POST", "/api/session/ses_a/fork", 404)]).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let refused = Runner::new(client(&endpoint)?)
        .with_fork(true)
        .run_prompt(
            SessionSelector::Id {
                session: "ses_a".parse()?,
                project: None,
            },
            "go".to_owned(),
            &mut output,
        )
        .await;
    match refused {
        Err(RunError::Fork { session, .. }) => assert_eq!(session.to_string(), "ses_a"),
        other => return Err(format!("the refused fork was not named: {other:?}").into()),
    }
    assert_eq!(server.await??, ["POST /api/session/ses_a/fork"]);
    Ok(())
}

/// A resume with nothing to resume starts a new Session, as the TypeScript
/// `run` did (session-target.ts), with or without `--fork`; a new Session is
/// never forked.
#[tokio::test]
async fn an_empty_resume_starts_a_new_session_and_never_forks() -> TestResult {
    for fork in [false, true] {
        let mut scripts = vec![
            Script::json(
                "GET",
                "/api/session?parentID=null&order=desc",
                200,
                r#"{"data":[],"cursor":{}}"#,
            ),
            Script::json("POST", "/api/session", 200, &session_record("ses_n", None)),
        ];
        scripts.extend(settled_run("ses_n"));
        let (receipt, requests) =
            run_with(fork, SessionSelector::ResumeLast { project: None }, scripts)
                .await
                .map_err(|error| {
                    format!("CCM117 an empty resume was refused (fork {fork}): {error}")
                })?;
        assert_eq!(receipt.exit, ExitClass::Success);
        assert_eq!(receipt.session.to_string(), "ses_n");
        assert!(
            !requests.iter().any(|request| request.ends_with("/fork")),
            "a new Session was forked: {requests:?}"
        );
    }
    Ok(())
}

/// `--project` scopes an empty resume as it scopes `-s`: with no Session in
/// the project, the new Session is created IN that project (at its worktree),
/// never outside it; a project the server does not know starts nothing.
#[tokio::test]
async fn an_empty_project_scoped_resume_starts_its_session_in_that_project() -> TestResult {
    const PROJECTS: &str =
        r#"[{"id":"prj_p","worktree":"/w/p","time":{"created":1,"updated":1},"sandboxes":[]}]"#;
    let empty = r#"{"data":[],"cursor":{}}"#;
    let resume_in = |project: &str| -> TestResult<SessionSelector> {
        Ok(SessionSelector::ResumeLast {
            project: Some(grapt_types::ProjectId::new(project.to_owned()).ok_or("project id")?),
        })
    };
    let mut scripts = vec![
        Script::json("GET", "/api/session?parentID=null&order=desc", 200, empty),
        Script::json("GET", "/api/project", 200, PROJECTS),
        Script::json(
            "POST",
            "/api/session",
            200,
            &session_record_in("ses_p", None, "prj_p", "/w/p"),
        )
        .expect_json(serde_json::json!({"location": {"directory": "/w/p"}})),
    ];
    scripts.extend(settled_run("ses_p"));
    let (receipt, _requests) = run_with(false, resume_in("prj_p")?, scripts)
        .await
        .map_err(|error| {
            format!("CCM119 the scoped empty resume did not start in its project: {error}")
        })?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert_eq!(receipt.session.to_string(), "ses_p");

    let scripts = vec![
        Script::json("GET", "/api/session?parentID=null&order=desc", 200, empty),
        Script::json("GET", "/api/project", 200, PROJECTS),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let unknown = Runner::new(client(&endpoint)?)
        .run_prompt(resume_in("prj_unknown")?, "go".to_owned(), &mut output)
        .await;
    assert!(
        matches!(unknown, Err(RunError::NoSession)),
        "an unknown project started a Session: {unknown:?}"
    );
    assert_eq!(
        server.await??.len(),
        2,
        "an unknown project created a Session"
    );
    Ok(())
}

/// A scoped empty resume whose created Session the server placed OUTSIDE the
/// project refuses `WrongProject`, naming both projects, and prompts nothing.
#[tokio::test]
async fn a_scoped_empty_resume_refuses_a_session_created_outside_its_project() -> TestResult {
    const PROJECTS: &str =
        r#"[{"id":"prj_p","worktree":"/w/p","time":{"created":1,"updated":1},"sandboxes":[]}]"#;
    let scripts = vec![
        Script::json(
            "GET",
            "/api/session?parentID=null&order=desc",
            200,
            r#"{"data":[],"cursor":{}}"#,
        ),
        Script::json("GET", "/api/project", 200, PROJECTS),
        Script::json(
            "POST",
            "/api/session",
            200,
            &session_record_in("ses_x", None, "prj_other", "/w/p"),
        ),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let refused = Runner::new(client(&endpoint)?)
        .run_prompt(
            SessionSelector::ResumeLast {
                project: Some(grapt_types::ProjectId::new("prj_p".to_owned()).ok_or("project id")?),
            },
            "go".to_owned(),
            &mut output,
        )
        .await;
    match refused {
        Err(RunError::WrongProject {
            session,
            requested,
            found,
        }) => {
            assert_eq!(session.to_string(), "ses_x");
            assert_eq!(requested.as_str(), "prj_p");
            assert_eq!(found.as_str(), "prj_other");
        }
        other => {
            return Err(format!(
                "CCM120 a Session created outside its project was accepted: {other:?}"
            )
            .into())
        }
    }
    assert_eq!(server.await??.len(), 3, "the refused run prompted");
    Ok(())
}

/// New runs, including an empty unscoped resume, explicitly send the canonical
/// cwd. Attach keeps its service-location contract; it is not a new run.
#[tokio::test]
async fn new_runs_bind_to_the_canonical_caller_directory() -> TestResult {
    let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
    for resume in [false, true] {
        let mut scripts = Vec::new();
        if resume {
            scripts.push(Script::json(
                "GET",
                "/api/session?parentID=null&order=desc",
                200,
                r#"{"data":[],"cursor":{}}"#,
            ));
        }
        scripts.push(
            Script::json("POST", "/api/session", 200, &session_record("ses_n", None))
                .expect_json(serde_json::json!({"location":{"directory":cwd}})),
        );
        let selector = if resume {
            SessionSelector::ResumeLast { project: None }
        } else {
            SessionSelector::New
        };
        let (endpoint, server) = spawn_server(scripts).await?;
        let output = BoundedOutput::new(
            Vec::new(),
            Vec::new(),
            OutputMode::Text,
            CliLimits::production(),
        );
        let selected = Runner::new(client(&endpoint)?)
            .select_session(selector, output.receipt())
            .await;
        // Name a body mismatch from the server before reporting the consequent
        // client transport failure; that distinguishes the binding mutant.
        server.await??;
        assert_eq!(selected?.0.to_string(), "ses_n");
    }
    Ok(())
}

#[tokio::test]
async fn attaching_a_new_session_keeps_the_service_location() -> TestResult {
    let (endpoint, server) = spawn_server(vec![
        Script::json("POST", "/api/session", 200, &session_record("ses_n", None))
            .expect_json(serde_json::json!({})),
        Script::json(
            "GET",
            "/api/session/ses_n",
            200,
            &session_record("ses_n", None),
        ),
    ])
    .await?;
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Text,
        CliLimits::production(),
    );
    let receipt = Runner::new(client(&endpoint)?)
        .attach(SessionSelector::New, &mut output)
        .await?;
    assert!(!receipt.prompted);
    assert_eq!(receipt.session.to_string(), "ses_n");
    assert_eq!(
        server.await??,
        ["POST /api/session", "GET /api/session/ses_n"]
    );
    Ok(())
}
