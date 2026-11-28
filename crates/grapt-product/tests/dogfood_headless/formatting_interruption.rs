//! Formatter suspension cannot separate live effects from their undo receipts.
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const ORIGINAL: &str = "original\n";

fn fixture(label: &str) -> TestResult<(PathBuf, PathBuf)> {
    let service = root(&format!("{label}-service"));
    prepare(&service, 45_000);
    unattended(&service);
    let workspace = root(&format!("{label}-workspace"));
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("notes.fmt"), ORIGINAL)?;
    // Both commands rendezvous before the fast call can publish. Only the
    // consumer releases the slow formatter, after interrupt or fast publication.
    let command = if cfg!(windows) {
        let script = service.join("formatter.cmd");
        fs::write(
            &script,
            concat!(
                "@echo off\r\nset /p value=<\"%~1\"\r\n",
                "if \"%value%\"==\"slow\" goto slow\r\n",
                ":fastwait\r\nif not exist format-entered goto fastwait\r\ngoto format\r\n",
                ":slow\r\n>format-entered echo entered\r\n",
                ":wait\r\nif not exist format-release goto wait\r\n",
                ":format\r\n> \"%~1\" echo formatted-%value%\r\n",
            ),
        )?;
        vec![
            std::env::var("COMSPEC")?,
            "/d".into(),
            "/c".into(),
            script.to_str().ok_or("formatter script encoding")?.into(),
            "$FILE".into(),
        ]
    } else {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            concat!(
                "IFS= read -r value < \"$1\" || exit 1; ",
                "if [ \"$value\" = slow ]; then ",
                "printf entered > format-entered || exit 1; ",
                "while [ ! -f format-release ]; do sleep 0.01; done; ",
                "else while [ ! -f format-entered ]; do sleep 0.01; done; fi; ",
                "printf 'formatted-%s\\n' \"$value\" > \"$1\"",
            )
            .into(),
            "formatter".into(),
            "$FILE".into(),
        ]
    };
    let path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["formatter"] = serde_json::json!({"fixture":{
        "command":command,"extensions":[".fmt"]
    }});
    fs::write(path, serde_json::to_vec(&config)?)?;
    Ok((service, workspace))
}

fn wait_for(what: &str, mut observed: impl FnMut() -> bool) {
    let wait = PollDeadline::within(Duration::from_secs(8));
    assert!(
        wait.poll(Duration::from_millis(10), || observed().then_some(()))
            .is_some(),
        "{what} never happened ({wait})"
    );
}

fn entered(workspace: &Path) -> bool {
    match fs::metadata(workspace.join("format-entered")) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("formatter marker unreadable: {error}"),
    }
}

fn contents(workspace: &Path) -> String {
    match fs::read_to_string(workspace.join("notes.fmt")) {
        Ok(text) => text.replace("\r\n", "\n"),
        Err(error) => panic!("live file unreadable: {error}"),
    }
}

fn restore(address: SocketAddr, session: &str) -> TestResult {
    // Active execution removal can precede release of its workspace permit.
    // Retry only that precise busy refusal, never a broken restore chain.
    let wait = PollDeadline::within(Duration::from_secs(8));
    let selected = wait.poll(Duration::from_millis(10), || {
        let (status, bytes) = request(
            address, "POST", &format!("/api/session/{session}/undo/preview"), "{}",
        );
        let response = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(response) => response,
            Err(error) => panic!("undo preview was not JSON: {error}"),
        };
        let busy = status == 409 && response["_tag"] == "ConflictError"
            && response["resource"] == "execution"
            && response["message"] == "this workspace or a nested workspace has admitted work or file undo in progress";
        (!busy).then_some((status, response))
    });
    let (status, preview) = selected.ok_or("undo workspace did not settle")?;
    assert_eq!(status, 200, "undo preview refused: {preview}");
    let receipt = json(address, "POST", &format!("/api/session/{session}/undo"),
        &serde_json::json!({"inputID":preview["data"]["inputID"],"target":preview["data"]["target"]}).to_string());
    assert_eq!(receipt["data"]["status"], "applied", "{receipt}");
    Ok(())
}

#[test]
fn interrupting_a_blocked_formatter_keeps_original_bytes_or_a_working_undo() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("formatter-interrupt")?;
    let provider = ProviderFixture::start_tolerating_cuts(vec![
        Script::ToolInput {
            call: "call_held_format",
            tool: "edit",
            arguments: serde_json::json!({"path":"notes.fmt","old_text":ORIGINAL,"new_text":"slow\n"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit the file"}"#,
    );
    wait_for("blocked formatter entry", || entered(&workspace));
    assert_eq!(
        contents(&workspace),
        ORIGINAL,
        "formatter suspension exposed unreceipted bytes"
    );
    let active = json(address, "GET", "/api/session/active", "");
    let execution = active["data"][&session]["executionID"]
        .as_str()
        .ok_or("held execution identity")?;
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/interrupt"),
        &serde_json::json!({"executionID":execution}).to_string(),
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    fs::write(workspace.join("format-release"), "release")?;
    wait_for("interrupted execution settlement", || {
        json(address, "GET", "/api/session/active", "")["data"][&session].is_null()
    });
    // Cancellation may race synchronous publication. Changed bytes are safe
    // only if the public undo consumer can restore the exact original.
    if contents(&workspace) != ORIGINAL {
        restore(address, &session)?;
    }
    assert_eq!(fs::read(workspace.join("notes.fmt"))?, ORIGINAL.as_bytes());
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
    Ok(())
}

#[test]
fn overlapping_same_turn_edits_keep_receipts_in_live_effect_order_for_undo() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("formatter-overlap")?;
    let mut frames = Vec::new();
    for (call, tool, arguments) in [
        (
            "call_slow_edit",
            "edit",
            serde_json::json!({"path":"notes.fmt","old_text":ORIGINAL,"new_text":"slow\n"}),
        ),
        (
            "call_fast_write",
            "write",
            serde_json::json!({"path":"notes.fmt","content":"fast\n"}),
        ),
    ] {
        frames.extend([
            serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":format!("item_{call}"),"call_id":call,"name":tool}}),
            serde_json::json!({"type":"response.function_call_arguments.delta","item_id":format!("item_{call}"),"delta":arguments.to_string()}),
            serde_json::json!({"type":"response.function_call_arguments.done","item_id":format!("item_{call}")}),
        ]);
    }
    frames.push(serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}));
    let body = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect::<String>();
    let provider = ProviderFixture::start(vec![Script::Raw(body), Script::Success]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"apply both edits"}"#,
    );
    wait_for("blocked formatter entry", || entered(&workspace));
    wait_for("overlapping fast publication", || {
        contents(&workspace) == "formatted-fast\n"
    });
    fs::write(workspace.join("format-release"), "release")?;
    wait_for("overlapping execution settlement", || {
        json(address, "GET", "/api/session/active", "")["data"][&session].is_null()
    });
    assert_eq!(
        contents(&workspace),
        "formatted-fast\n",
        "stale slow edit replaced the newer effect"
    );
    restore(address, &session)?;
    assert_eq!(fs::read(workspace.join("notes.fmt"))?, ORIGINAL.as_bytes());
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_path_alias_to_an_external_formatter_is_not_launch_authority() -> TestResult {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let _journey = process_journey_custody();
    let (service, workspace) = fixture("formatter-path-alias")?;
    let bin = workspace.join("bin");
    fs::create_dir(&bin)?;
    let executable = service.join("external-formatter");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf launched > formatter-launched || exit 1\nprintf 'unexpected\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    symlink(&executable, bin.join("formatter"))?;
    // Canonicalizing only the executable would hide its repository-owned alias.
    assert!(!fs::canonicalize(bin.join("formatter"))?.starts_with(fs::canonicalize(&workspace)?));
    let path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["formatter"] = serde_json::json!({"fixture":{
        "command":["formatter","$FILE"],"extensions":[".fmt"],"environment":{"PATH":bin}
    }});
    fs::write(path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_path_alias",
            tool: "edit",
            arguments: serde_json::json!({"path":"notes.fmt","old_text":ORIGINAL,"new_text":"raw edit\n"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit the file without running workspace executables"}"#,
    );
    wait_for("PATH alias execution settlement", || {
        json(address, "GET", "/api/session/active", "")["data"][&session].is_null()
    });
    assert_eq!(fs::read(workspace.join("notes.fmt"))?, b"raw edit\n");
    let output = permission_boundaries::output(&provider, "call_path_alias");
    let result: serde_json::Value = serde_json::from_str(&output)?;
    let feedback = result["formatting"]
        .as_array()
        .ok_or("formatter feedback absent")?;
    assert!(
        feedback.iter().any(|entry| entry["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())),
        "formatter launch refusal absent: {output}"
    );
    assert!(
        !workspace.join("formatter-launched").try_exists()?,
        "workspace PATH alias launched"
    );
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
    Ok(())
}

#[cfg(unix)]
#[test]
fn formatter_launch_uses_canonical_image_through_a_workspace_redirect() -> TestResult {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let _journey = process_journey_custody();
    let (service, workspace) = fixture("formatter-image-binding")?;
    let bin = service.join("trusted-bin");
    let tools = service.join("trusted-tools");
    fs::create_dir(&bin)?;
    fs::create_dir(&tools)?;
    let executable = tools.join("formatter");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s' \"$0\" > launch-image || exit 1\ntest -f \"$1\"\n",
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let redirect = workspace.join("redirect");
    symlink(&executable, &redirect)?;
    symlink(&redirect, bin.join("formatter"))?;
    let image = fs::canonicalize(&executable)?;
    let candidate = fs::canonicalize(&bin)?.join("formatter");
    assert_eq!(fs::canonicalize(&candidate)?, image);
    let path = service.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["formatter"] = serde_json::json!({"fixture":{
        "command":["formatter","$FILE"],"extensions":[".fmt"],"environment":{"PATH":bin}
    }});
    fs::write(path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_bound_image",
            tool: "edit",
            arguments: serde_json::json!({"path":"notes.fmt","old_text":ORIGINAL,"new_text":"edited staging bytes\n"}),
        },
        Script::Success,
    ]);
    let mut child = spawn(&service, provider.address);
    let address = await_ready(&service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"edit the file with the configured formatter"}"#,
    );
    wait_for("bound formatter image execution settlement", || {
        json(address, "GET", "/api/session/active", "")["data"][&session].is_null()
    });
    assert_eq!(
        fs::read(workspace.join("notes.fmt"))?,
        b"edited staging bytes\n"
    );
    // A shebang interpreter exposes the spawned script image in $0, not the
    // separately supplied dispatch name. No alias mutation or timing race is needed.
    let launched = PathBuf::from(fs::read_to_string(workspace.join("launch-image"))?);
    assert_eq!(
        launched, image,
        "formatter did not spawn its validated image"
    );
    assert_ne!(
        launched, candidate,
        "formatter spawned the repository-routed alias"
    );
    let output = permission_boundaries::output(&provider, "call_bound_image");
    let result: serde_json::Value = serde_json::from_str(&output)?;
    let feedback = result["formatting"]
        .as_array()
        .ok_or("formatter feedback absent")?;
    assert!(
        feedback.iter().any(|entry| entry["formatted"] == true),
        "formatter did not succeed: {output}"
    );
    child.stop_after_witness(&service);
    drop(child);
    process_fixture::cleanup(&workspace);
    process_fixture::cleanup(&service);
    Ok(())
}
