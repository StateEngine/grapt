//! PROJECTS lists the projects a user's Sessions live in, newest first, each
//! at its project root, and never the profile's own service root.
use super::*;

/// A Git repository with one commit whose message makes its root commit,
/// and so its project identity, its own.
fn repository(path: &Path, message: &str) {
    fs::create_dir_all(path.join("sub")).unwrap_or_else(|error| panic!("{error}"));
    for arguments in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        ],
    ] {
        let mut git = Command::new("git");
        git.env("GIT_CEILING_DIRECTORIES", std::env::temp_dir());
        git.args(&arguments).current_dir(path);
        let (code, _, stderr) = bounded(git, Duration::from_secs(30));
        assert_eq!(
            code,
            Some(0),
            "git {arguments:?} failed in {}: {stderr}",
            path.display()
        );
    }
}

/// The most output a fixture command may produce on one stream.
const OUTPUT_CAP: u64 = 16 * 1024 * 1024;

/// Run `command` to completion within `limit`, its stdout and stderr in two
/// fresh files. The wait is bounded: past the limit the direct child is
/// killed and reaped, and the fixture fails naming the command.
pub(super) fn bounded(mut command: Command, limit: Duration) -> (Option<i32>, String, String) {
    static ORDINAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let ordinal = ORDINAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "grapt-fixture-command-{}-{ordinal}",
        std::process::id()
    ));
    let (out_path, err_path) = (base.with_extension("out"), base.with_extension("err"));
    let create = |path: &Path| {
        fs::File::create(path).unwrap_or_else(|error| panic!("fixture output file: {error}"))
    };
    let name = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(create(&out_path))
        .stderr(create(&err_path))
        .spawn()
        .unwrap_or_else(|error| panic!("{name} could not start: {error}"));
    let deadline = now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{name} did not finish within {limit:?}");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{name} wait failed: {error}");
            }
        }
    };
    let read = |path: &Path| {
        let file = fs::File::open(path).unwrap_or_else(|error| panic!("fixture output: {error}"));
        let mut text = Vec::new();
        file.take(OUTPUT_CAP + 1)
            .read_to_end(&mut text)
            .unwrap_or_else(|error| panic!("fixture output unreadable: {error}"));
        let _ = fs::remove_file(path);
        assert!(
            text.len() as u64 <= OUTPUT_CAP,
            "{name} output exceeded {OUTPUT_CAP} bytes"
        );
        String::from_utf8_lossy(&text).into_owned()
    };
    let (stdout, stderr) = (read(&out_path), read(&err_path));
    (status.code(), stdout, stderr)
}

/// This fixture's wall-clock deadlines.
#[allow(clippy::disallowed_methods)]
fn now() -> std::time::Instant {
    std::time::Instant::now()
}

fn create_at(address: SocketAddr, directory: &Path) -> String {
    let body = serde_json::json!({"location": {"directory": directory}});
    json(address, "POST", "/api/session", &body.to_string())["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created session had no id"))
        .to_owned()
}

/// The listed (id, worktree) pairs, after asserting none is the service root
/// or inside it.
fn projects(address: SocketAddr, root: &Path) -> Vec<(String, String)> {
    let listed = json(address, "GET", "/api/project", "");
    let rows = listed
        .as_array()
        .unwrap_or_else(|| panic!("project list is not an array: {listed}"));
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap_or_default().to_owned(),
                row["worktree"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    for (_, worktree) in &pairs {
        assert!(
            !Path::new(worktree).starts_with(root),
            "the service root was listed as a project: {listed}"
        );
    }
    pairs
}

#[test]
fn projects_are_where_sessions_live_and_never_the_service_root() {
    let _journey = process_journey_custody();
    let root = root("project-list");
    prepare(&root, 200_000);
    let repos = PathBuf::from(format!("{}-repos", root.display()));
    let _ = fs::remove_dir_all(&repos);
    let (a, b) = (repos.join("a"), repos.join("b"));
    repository(&a, "project a");
    repository(&b, "project b");
    let provider = ProviderFixture::start(vec![Script::SuccessText("resumed in a")]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);

    // A fresh profile lists nothing; a Session placed in the service root
    // still makes no project of it.
    let fresh = projects(address, &root);
    let _in_root = create(address);
    let after_root = projects(address, &root);

    create_at(address, &a);
    create_at(address, &b);
    let two = projects(address, &root);
    // A newer Session deeper in A moves A to the front, still at its root.
    let newest_a = create_at(address, &a.join("sub"));
    let reordered = projects(address, &root);
    // The CLI resumes a project by the id PROJECTS lists: A's newest Session.
    let service = root.to_string_lossy().into_owned();
    let a_id = reordered
        .first()
        .map(|(id, _)| id.clone())
        .unwrap_or_default();
    let mut cli = Command::new(super::cli_continuity::cli_binary());
    cli.args([
        "run",
        "--service",
        &service,
        "--resume",
        "--project",
        &a_id,
        "--prompt",
        "continue",
        "--jsonl",
    ]);
    let (code, stdout, stderr) = bounded(cli, Duration::from_secs(120));
    let resumed = super::cli_continuity::CliRun {
        code,
        stdout,
        stderr,
    };
    let resumed_session = resumed.receipt()["session"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let resumed_directory = json(
        address,
        "GET",
        &format!("/api/session/{resumed_session}"),
        "",
    )["data"]["location"]["directory"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let _ = fs::remove_dir_all(&repos);

    assert!(
        fresh.is_empty(),
        "a fresh profile listed projects: {fresh:?}"
    );
    assert!(
        after_root.is_empty(),
        "a service-root Session made a project: {after_root:?}"
    );
    let (a, b) = (a.display().to_string(), b.display().to_string());
    assert_eq!(two.len(), 2, "{two:?}");
    assert_eq!(
        (two[0].1.as_str(), two[1].1.as_str()),
        (b.as_str(), a.as_str()),
        "{two:?}"
    );
    assert_ne!(
        two[0].0, two[1].0,
        "two repositories shared a project: {two:?}"
    );
    assert_eq!(reordered.len(), 2, "{reordered:?}");
    assert_eq!(
        (reordered[0].1.as_str(), reordered[1].1.as_str()),
        (a.as_str(), b.as_str()),
        "{reordered:?}"
    );
    assert_eq!(
        reordered[0].0, two[1].0,
        "A's identity changed: {reordered:?}"
    );
    assert_eq!(resumed.code, Some(0), "resume failed: {}", resumed.stderr);
    assert_eq!(resumed_session, newest_a, "the CLI resumed another Session");
    assert!(
        Path::new(&resumed_directory).starts_with(&a),
        "the resumed Session is outside A: {resumed_directory}"
    );
}
