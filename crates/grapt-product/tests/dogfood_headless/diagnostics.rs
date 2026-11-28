//! Diagnostics through provider-visible edit results and a real stdio server.
use super::*;
use serde_json::{json, Value};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const SERVER: &str = r#"
import json, os, sys, time
if len(sys.argv) == 3 and sys.argv[1] == '--format':
    with open(sys.argv[2], 'w', newline='') as output:
        output.write('formatted\n')
    sys.exit(0)
def send(message):
    body = json.dumps(dict(jsonrpc='2.0', **message)).encode('utf-8')
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n' % len(body)).encode('ascii') + body)
    sys.stdout.buffer.flush()
def publish(document, items, version):
    params = dict(uri=document['uri'], diagnostics=items)
    if version is not None:
        params['version'] = version
    send(dict(method='textDocument/publishDiagnostics', params=params))
document = None
while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(1)
        if line == b'\r\n':
            break
        key, value = line.decode('ascii').split(':', 1)
        headers[key.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method = message.get('method')
    if method == 'initialize':
        assert message['id'] == 1
        send(dict(id=1, result=dict(capabilities=dict(textDocumentSync=1))))
    elif method == 'textDocument/didOpen':
        document = message['params']['textDocument']
        text, version = document['text'], document['version']
        if text.startswith('silent'):
            continue
        if text.startswith('burst'):
            for _ in range(32):
                send(dict(method='window/logMessage', params=dict(type=3, message='x' * 4096)))
        summary = text.split('\n', 1)[0] if text.startswith(('burst', 'large')) else text
        item = dict(severity=1, message=summary, range=dict(
            start=dict(line=0, character=0), end=dict(line=0, character=1)))
        if text.startswith('invalid'):
            item['range']['start']['character'] = 2
        if text.startswith('stale'):
            publish(document, [dict(item, message='must-not-surface')], version - 1)
        publish(document, [] if text.startswith('clean') else [item],
                None if text.startswith('unversioned') else version)
    elif method == 'shutdown':
        assert message['id'] == 2
        send(dict(id=2, result=None))
    elif method == 'exit':
        if document['text'].startswith('graceful'):
            os.close(sys.stderr.fileno())
            time.sleep(0.2)
        with open(os.environ['LSP_EXIT_MARKER'], 'a', encoding='utf-8') as marker:
            marker.write(json.dumps(document) + '\n')
        sys.exit(0)
"#;

fn fixture(label: &str, formatted: bool) -> TestResult<(PathBuf, PathBuf)> {
    let service = root(&format!("diagnostics-{label}-service"));
    let workspace = root(&format!("diagnostics-{label}-workspace"));
    prepare(&service, 120_000);
    unattended(&service);
    process_fixture::cleanup(&workspace);
    fs::create_dir_all(&workspace)?;
    let script = service.join("fixture.py");
    fs::write(&script, SERVER)?;
    let path = service.join("grapt.jsonc");
    let mut config: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let python = if cfg!(windows) { "python" } else { "python3" };
    config["lsp"] = json!({"fixture":{
        "command":[python,script],"extensions":[".lsp"],
        "env":{"LSP_EXIT_MARKER":service.join("exits.jsonl")}
    }});
    if formatted {
        let command = if cfg!(windows) {
            let script = service.join("formatter.cmd");
            fs::write(&script, "@echo off\r\n> \"%~1\" echo formatted\r\n")?;
            json!([std::env::var("COMSPEC")?, "/d", "/c", script, "$FILE"])
        } else {
            json!([
                "/bin/sh",
                "-c",
                "printf 'formatted\\n' > \"$1\"",
                "formatter",
                "$FILE"
            ])
        };
        config["formatter"] = json!({"fixture":{"command":command,"extensions":[".lsp"]}});
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            // Both trusted scripts use env shebangs: interpreter lookup must reject repository PATH.
            fs::write(&script, format!("#!/usr/bin/env python3\n{SERVER}"))?;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
            let fake_bin = workspace.join("fake-bin");
            fs::create_dir(&fake_bin)?;
            let fake = fake_bin.join("python3");
            fs::write(
                &fake,
                "#!/bin/sh\nprintf hijacked > \"$LSP_PATH_MARKER\"\nexit 9\n",
            )?;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))?;
            let inherited = std::env::var_os("PATH").ok_or("PATH absent")?;
            let search = std::env::join_paths(
                std::iter::once(fake_bin).chain(std::env::split_paths(&inherited)),
            )?;
            config["shell_environment_policy"] = json!({"set":{
                "PATH":search.to_str().ok_or("PATH encoding")?,
                "LSP_PATH_MARKER":service.join("interpreter-launched")
            }});
            config["lsp"]["fixture"]["command"] = json!([script]);
            config["formatter"]["fixture"]["command"] = json!([script, "--format", "$FILE"]);
        }
    }
    fs::write(path, serde_json::to_vec(&config)?)?;
    Ok((service, workspace))
}

fn input(call: &'static str, tool: &'static str, arguments: Value) -> Script {
    Script::ToolInput {
        call,
        tool,
        arguments,
    }
}

fn run(service: &Path, workspace: &Path, mut scripts: Vec<Script>) -> TestResult<ProviderFixture> {
    scripts.push(Script::Success);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(service, provider.address);
    let address = await_ready(service, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &json!({"location":{"directory":workspace}}).to_string(),
    )["data"]["id"]
        .as_str()
        .ok_or("session identity")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"change the source files"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    child.stop_after_witness(service);
    Ok(provider)
}

fn feedback(provider: &ProviderFixture, call: &str) -> TestResult<Value> {
    let output = permission_boundaries::output(provider, call);
    assert!(!output.contains("Tool failed"), "{output}");
    let mut result: Value = serde_json::from_str(&output)?;
    assert!(result["diagnostics"].is_array(), "{output}");
    Ok(result["diagnostics"].take())
}

fn exits(service: &Path) -> TestResult<Vec<Value>> {
    fs::read_to_string(service.join("exits.jsonl"))?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

#[test]
fn diagnostics_observe_formatted_write_edit_and_apply_patch_bytes() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("formatted", true)?;
    let provider = run(
        &service,
        &workspace,
        vec![
            input(
                "write",
                "write",
                json!({"path":"notes.lsp","content":"raw\n"}),
            ),
            input(
                "edit",
                "edit",
                json!({"path":"notes.lsp","old_text":"formatted","new_text":"edited"}),
            ),
            input(
                "patch",
                "apply_patch",
                json!({"input":"*** Begin Patch\n*** Add File: patch.lsp\n+raw\n*** End Patch\n"}),
            ),
        ],
    )?;
    assert!(!service.join("interpreter-launched").try_exists()?);
    let observed_exits = exits(&service)?;
    assert_eq!(observed_exits.len(), 3);
    let mut previous = 0;
    for ((call, path), observed) in [
        ("write", "notes.lsp"),
        ("edit", "notes.lsp"),
        ("patch", "patch.lsp"),
    ]
    .into_iter()
    .zip(observed_exits)
    {
        let output = feedback(&provider, call)?;
        let observation = &output[0];
        let text = String::from_utf8(fs::read(workspace.join(path))?)?;
        assert_eq!(text.replace("\r\n", "\n"), "formatted\n");
        assert_eq!(observation["path"], path, "{output}");
        assert_eq!(observation["server"], "fixture", "{output}");
        assert_eq!(observation["status"], "reported", "{output}");
        assert_eq!(observation["versioned"], true, "{output}");
        let version = observation["version"].as_i64().ok_or("version absent")?;
        assert!(version > previous, "{output}");
        previous = version;
        assert_eq!(observation["items"][0]["message"], text, "{output}");
        assert_eq!(observation["items"][0]["severity"], 1, "{output}");
        assert_eq!(observed["text"], text);
        assert_eq!(observed["version"], version);
    }
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn diagnostics_distinguish_current_unversioned_clean_silent_and_invalid() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("modes", false)?;
    let modes = "stale unversioned clean silent invalid graceful".split_whitespace();
    let mut scripts: Vec<_> = modes
        .clone()
        .map(|mode| {
            input(
                mode,
                "write",
                json!({"path":format!("{mode}.lsp"),"content":mode}),
            )
        })
        .collect();
    let padding = "x".repeat(128 * 1024);
    for name in ["burst", "large"] {
        fs::write(
            workspace.join(format!("{name}.lsp")),
            format!("before\n{padding}\n"),
        )?;
    }
    let patch = "*** Begin Patch\n*** Update File: burst.lsp\n@@\n-before\n+burst\n*** Update File: large.lsp\n@@\n-before\n+large\n*** End Patch\n";
    scripts.push(input("burst", "apply_patch", json!({"input":patch})));
    let provider = run(&service, &workspace, scripts)?;
    let observed_exits = exits(&service)?;
    for mode in modes.chain(["burst", "large"]) {
        let large = matches!(mode, "burst" | "large");
        let call = if large { "burst" } else { mode };
        let output = feedback(&provider, call)?;
        let entries = output.as_array().ok_or("feedback absent")?;
        assert_eq!(entries.len(), if large { 2 } else { 1 }, "{output}");
        assert!(output.to_string().len() < 64 * 1024);
        let path = format!("{mode}.lsp");
        let observation = entries
            .iter()
            .find(|e| e["path"] == path)
            .ok_or("document absent")?;
        let text = if large {
            format!("{mode}\n{padding}\n")
        } else {
            mode.into()
        };
        assert_eq!(fs::read_to_string(workspace.join(path))?, text);
        assert_eq!(observation["server"], "fixture", "{output}");
        assert!(!output.to_string().contains("must-not-surface"), "{output}");
        if matches!(mode, "silent" | "invalid") {
            assert_eq!(observation["status"], "unavailable", "{output}");
            let reason = observation["reason"].as_str().ok_or("reason absent")?;
            assert!(!reason.is_empty());
            continue;
        }
        let witness = if large {
            format!("large\n{padding}\n")
        } else {
            text
        };
        assert!(observed_exits.iter().any(|e| e["text"] == witness));
        assert_eq!(observation["versioned"], mode != "unversioned", "{output}");
        let version = observation.get("version").ok_or("version absent")?;
        assert_eq!(version.is_null(), mode == "unversioned", "{output}");
        if mode != "unversioned" {
            assert!(version.as_i64().is_some_and(|v| v > 0));
        }
        let clean = mode == "clean";
        let status = if clean { "clean" } else { "reported" };
        assert_eq!(observation["status"], status, "{output}");
        let items = observation["items"].as_array().ok_or("items absent")?;
        assert_eq!(items.len(), usize::from(!clean));
        if !clean {
            assert_eq!(items[0]["message"], mode, "{output}");
        }
    }
    assert_eq!(observed_exits.len(), 5);
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
fn installed_servers_require_user_authority_before_edit_diagnostics() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("authority", false)?;
    let bin = service.join("bin");
    fs::create_dir(&bin)?;
    let marker = service.join("server-starts");
    let script = format!(
        "import os\nwith open(os.environ['LSP_START_MARKER'], 'a') as marker: marker.write('start\\n')\n{SERVER}"
    );
    #[cfg(windows)]
    {
        fs::write(service.join("fixture.py"), &script)?;
        fs::write(
            bin.join("rust-analyzer.cmd"),
            "@echo off\r\npython \"%~dp0..\\fixture.py\"\r\n",
        )?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = bin.join("rust-analyzer");
        fs::write(&path, format!("#!/usr/bin/env python3\n{script}"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let inherited = std::env::var_os("PATH").ok_or("PATH absent")?;
    let search =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited)))?;
    let path = service.join("grapt.jsonc");
    let mut config: Value = serde_json::from_slice(&fs::read(&path)?)?;
    config
        .as_object_mut()
        .ok_or("service configuration")?
        .remove("lsp");
    config["shell_environment_policy"] = json!({"set":{
        "PATH":search.to_str().ok_or("PATH encoding")?,
        "LSP_START_MARKER":marker,
        "LSP_EXIT_MARKER":service.join("exits.jsonl")
    }});
    for index in 0..3 {
        let enabled = index != 0;
        if index == 1 {
            config["lsp"] = json!({"rust-analyzer":{"disabled":false}});
        } else if index == 2 {
            config["lsp"] = json!(true);
        }
        fs::write(&path, serde_json::to_vec(&config)?)?;
        let provider = run(
            &service,
            &workspace,
            vec![input(
                "authority",
                "write",
                json!({"path":"probe.rs","content":format!("source-{index}")}),
            )],
        )?;
        assert_eq!(marker.try_exists()?, enabled);
        if enabled {
            assert_eq!(
                feedback(&provider, "authority")?[0]["server"],
                "rust-analyzer"
            );
            let observed = exits(&service)?;
            assert_eq!(observed.len(), index);
            assert_eq!(observed[index - 1]["text"], format!("source-{index}"));
        } else {
            let result: Value =
                serde_json::from_str(&permission_boundaries::output(&provider, "authority"))?;
            assert!(result.get("diagnostics").is_none());
            assert_eq!(fs::read(workspace.join("probe.rs"))?, b"source-0");
        }
    }
    fs::write(
        workspace.join("grapt.json"),
        r#"{"lsp":{"rust-analyzer":{"disabled":true}}}"#,
    )?;
    let provider = run(
        &service,
        &workspace,
        vec![input(
            "restricted",
            "edit",
            json!({"path":"probe.rs","old_text":"source-2","new_text":"retained"}),
        )],
    )?;
    let result: Value =
        serde_json::from_str(&permission_boundaries::output(&provider, "restricted"))?;
    assert!(result.get("diagnostics").is_none());
    assert_eq!(fs::read(workspace.join("probe.rs"))?, b"retained");
    assert_eq!(fs::read(marker)?, b"start\nstart\n");
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}

#[test]
fn repository_lsp_command_is_refused_without_losing_the_edit() -> TestResult {
    let _journey = process_journey_custody();
    let (service, workspace) = fixture("project", false)?;
    fs::write(
        workspace.join("grapt.json"),
        r#"{"lsp":{"untrusted":{"command":["must-not-launch"],"extensions":[".lsp"]}}}"#,
    )?;
    let provider = run(
        &service,
        &workspace,
        vec![input(
            "project",
            "write",
            json!({"path":"notes.lsp","content":"retained"}),
        )],
    )?;
    assert_eq!(fs::read(workspace.join("notes.lsp"))?, b"retained");
    let output = feedback(&provider, "project")?;
    assert_eq!(output[0]["status"], "unavailable", "{output}");
    let reason = output[0]["reason"].as_str().ok_or("reason absent")?;
    assert!(reason.contains("project lsp configuration"), "{output}");
    process_fixture::cleanup(&service);
    process_fixture::cleanup(&workspace);
    Ok(())
}
