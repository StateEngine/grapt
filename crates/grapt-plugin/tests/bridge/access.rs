//! Declarations reach the real bridge and its OS-contained process.
use super::*;
use std::io::{Read as _, Write as _};

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn plugin_io_requires_its_declared_roots_and_network_origin() -> Result<(), Box<dyn Error>> {
    let readable = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    let allowed_file = readable.path().join("allowed.txt");
    let private_file = private.path().join("private.txt");
    std::fs::write(&allowed_file, "declared")?;
    std::fs::write(&private_file, "undeclared")?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = thread::spawn(move || -> std::io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && tokio::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if request.len() >= 4096 || stream.read(&mut byte)? == 0 {
                return Err(std::io::Error::other(
                    "HTTP fixture received incomplete or oversized headers",
                ));
            }
            request.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\ndeclared")
    });
    let (root, _plugin) = fixture(
        "declared-io",
        r#"
import fs from "node:fs"
export default { id: "fixture.access", async setup(ctx) {
  await ctx.tool.transform(draft => draft.add({name:"io",args:{type:"object"},async execute(input) {
    const refused = async (action, reason) => {
      try { await action(); return false } catch (error) { return reason === undefined || error.message.startsWith(reason) }
    }
    const ownWriteDenied = await refused(() => fs.writeFileSync(new URL("./own.txt", import.meta.url), "own"))
    const tmpFile = `${process.env.TMPDIR || process.env.TEMP}/plugin-scratch.txt`
    fs.writeFileSync(tmpFile, "scratch")
    return {
      ownWriteDenied,
      scratch: fs.readFileSync(tmpFile, "utf8"),
      declared: fs.readFileSync(input.allowed, "utf8"),
      readDenied: await refused(() => fs.readFileSync(input.private, "utf8")),
      writeDenied: await refused(() => fs.writeFileSync(input.allowed, "overwritten")),
      fetched: await (await fetch(input.origin)).text(),
      networkDenied: await refused(() => fetch("http://127.0.0.1:1")),
      metadataDenied: await refused(() => fetch(input.metadataUrl), "plugin_network_blocked:"),
      socketDenied: await refused(() => import("node:net")),
      globalSocketDenied: await refused(() => new WebSocket("ws://127.0.0.1:1"), "plugin_network_api_denied:"),
    }
  }}))
}}
"#,
    )?;
    let mut resolved = resolve_declarations(&[ConfigDocument {
        directory: root.clone(),
        source: "explicit-fixture".into(),
        origin: PluginOrigin::LaunchExplicit,
        value: serde_json::json!({"plugins":[{"package":"./fixture.mjs","permissions":{
            "read":[readable.path()]
        }}]}),
    }])?;
    assert!(resolved.blocked.is_empty(), "{:?}", resolved.blocked);
    resolved
        .active
        .first_mut()
        .ok_or("active plugin")?
        .permissions
        .allow_loopback_origin_for_tests(&origin)?;
    let (runtime, _cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, resolved.active, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let metadata_url = format!(
        "http://{}/latest/meta-data/",
        std::net::Ipv4Addr::new(169, 254, 169, 254)
    );
    let result = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({"allowed":allowed_file,"private":private_file,"origin":origin,"metadataUrl":metadata_url}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(
        result.get("scratch").and_then(serde_json::Value::as_str),
        Some("scratch")
    );
    assert_eq!(
        result.get("declared").and_then(serde_json::Value::as_str),
        Some("declared")
    );
    assert_eq!(
        result.get("fetched").and_then(serde_json::Value::as_str),
        Some("declared")
    );
    for key in [
        "ownWriteDenied",
        "readDenied",
        "writeDenied",
        "networkDenied",
        "metadataDenied",
        "socketDenied",
        "globalSocketDenied",
    ] {
        assert_eq!(
            result.get(key).and_then(serde_json::Value::as_bool),
            Some(true),
            "{key}: {result}"
        );
    }
    assert_eq!(std::fs::read_to_string(allowed_file)?, "declared");
    server.join().map_err(|_| "HTTP fixture panicked")??;
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_plugin_policy_denies_io_without_node_permission_flags() -> Result<(), Box<dyn Error>> {
    let own = tempfile::tempdir()?;
    let private = tempfile::tempdir()?;
    let secret = private.path().join("private.txt");
    std::fs::write(&secret, "private")?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let mut environment = ["SystemRoot", "WINDIR"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| format!("{name}={value}"))
        })
        .collect::<Vec<_>>();
    environment.push(format!("TMPDIR={}", own.path().display()));
    let executable = node()?;
    let request = grapt_sandbox::ProcessRequest {
        executable: executable.clone(),
        arguments: vec![
            "-e".into(),
            r#"
const fs = require("node:fs")
const net = require("node:net")
const deadline = setTimeout(() => process.exit(2), 3000)
fs.writeFileSync("own.txt", "own")
const own = fs.readFileSync("own.txt", "utf8")
let readDenied = false
try { fs.readFileSync(process.argv[1]) } catch { readDenied = true }
let writeDenied = false
try { fs.writeFileSync(process.argv[1], "overwritten") } catch { writeDenied = true }
const socket = net.createConnection({host:"127.0.0.1",port:Number(process.argv[2])})
const done = networkDenied => {
  socket.destroy()
  clearTimeout(deadline)
  process.stdout.write(JSON.stringify({own,readDenied,writeDenied,networkDenied}))
}
socket.once("connect", () => done(false))
socket.once("error", () => done(true))
"#
            .into(),
            secret.to_string_lossy().into_owned(),
            port.to_string(),
        ],
        working_directory: own.path().to_path_buf(),
        environment,
        policy: grapt_sandbox::Policy {
            readable_roots: vec![executable],
            writable_roots: vec![own.path().to_path_buf()],
            protected_paths: vec![],
            denied_paths: vec![],
            network: grapt_sandbox::NetworkAccess::Off,
        },
    };
    #[cfg(target_os = "linux")]
    let (success, status, bytes, diagnostics) = {
        let launch = grapt_sandbox::PipedCommand::prepare(&request)?;
        let mut command = Command::new("/usr/bin/env");
        command.arg("-i");
        launch.append_to(&mut command);
        let output = command.output()?;
        (
            output.status.success(),
            output.status.to_string(),
            output.stdout,
            output.stderr,
        )
    };
    #[cfg(windows)]
    let (success, status, bytes, diagnostics) = {
        let mut child = grapt_sandbox::spawn_piped(&request)?;
        let output = child.take_stdout().ok_or("native stdout is missing")?;
        drop(child.take_stdin());
        let diagnostics = child.take_stderr().ok_or("native stderr is missing")?;
        let drain = thread::spawn(move || {
            let mut bytes = Vec::new();
            diagnostics
                .take(4097)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let status = child.wait()?;
        let mut bytes = Vec::new();
        output.take(4097).read_to_end(&mut bytes)?;
        let diagnostics = drain
            .join()
            .map_err(|_| "native diagnostic reader panicked")??;
        (status == 0, format!("exit {status:#x}"), bytes, diagnostics)
    };
    assert!(
        success,
        "native policy fixture {status}: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    let result: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        result.get("own").and_then(serde_json::Value::as_str),
        Some("own")
    );
    for key in ["readDenied", "writeDenied", "networkDenied"] {
        assert_eq!(
            result.get(key).and_then(serde_json::Value::as_bool),
            Some(true),
            "{key}: {result}"
        );
    }
    assert_eq!(std::fs::read_to_string(secret)?, "private");
    Ok(())
}

#[test]
fn killing_a_generation_closes_its_stalled_http_exchange() -> Result<(), Box<dyn Error>> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let origin = format!("http://{}", listener.local_addr()?);
    let (accepted, observed) = mpsc::sync_channel(1);
    let peer = thread::spawn(move || -> std::io::Result<bool> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && tokio::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if headers.len() >= 4096 || stream.read(&mut byte)? == 0 {
                return Err(std::io::Error::other("incomplete fixture request"));
            }
            headers.push(byte[0]);
        }
        accepted.send(()).map_err(std::io::Error::other)?;
        let mut byte = [0];
        Ok(stream.read(&mut byte)? == 0)
    });
    let (root, _) = fixture(
        "network-retirement",
        r#"export default {id:"fixture.network_retirement",async setup(ctx) {
          await ctx.tool.transform(draft => draft.add({name:"wait",args:{type:"object"},
            async execute(input) { return (await fetch(input.url)).text() }
          }))
        }}"#,
    )?;
    let _cleanup = RootCleanup(root.clone());
    let mut resolved = resolve_declarations(&[ConfigDocument {
        directory: root,
        source: "explicit-fixture".into(),
        origin: PluginOrigin::LaunchExplicit,
        value: serde_json::json!({"plugins":[{"package":"./fixture.mjs"}]}),
    }])?;
    resolved
        .active
        .first_mut()
        .ok_or("active plugin")?
        .permissions
        .allow_loopback_origin_for_tests(&origin)?;
    let (runtime, _runtime_cleanup) = runtime()?;
    let supervisor = Arc::new(PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?);
    let generation = gated_activate(&supervisor, resolved.active, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let route = (
        generation.id(),
        registration.plugin_id.clone(),
        tool.callback,
    );
    let invoked = Arc::clone(&supervisor);
    let (finished, outcome) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = invoked.invoke(
            route.0,
            &route.1,
            route.2,
            serde_json::json!({"url":origin}),
            serde_json::json!({}),
            CallAuthority::Unscoped,
        );
        let _ = finished.send(result);
    });
    let started = observed.recv_timeout(Duration::from_secs(20));
    let killed = supervisor.cancel_generation(generation.id());
    let result = outcome.recv_timeout(Duration::from_secs(3))?;
    worker.join().map_err(|_| "invocation worker panicked")?;
    started?;
    assert!(killed?, "generation was absent before cancellation");
    assert!(result.is_err(), "killed request reported success");
    assert!(
        peer.join().map_err(|_| "HTTP fixture panicked")??,
        "broker connection stayed open after its guest was killed"
    );
    assert_eq!(supervisor.live_processes(), 0);
    assert!(supervisor.current()?.is_none());
    Ok(())
}

#[test]
fn plugin_network_declarations_block_private_loopback_and_metadata_origins(
) -> Result<(), Box<dyn Error>> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let (root, _) = fixture(
        "ssrf-blocked",
        r#"export default { id: "fixture.ssrf", async setup() {} }"#,
    )?;
    let _cleanup = RootCleanup(root.clone());
    for blocked in [
        format!("http://{}:8080", Ipv4Addr::LOCALHOST),
        format!("http://[{}]:8080", Ipv6Addr::LOCALHOST),
        format!("http://{}", Ipv4Addr::new(169, 254, 169, 254)),
        format!("http://{}", Ipv4Addr::new(10, 0, 0, 1)),
        format!("http://{}", Ipv4Addr::new(172, 16, 0, 1)),
        format!("http://{}", Ipv4Addr::new(192, 168, 1, 1)),
        "https://metadata.example.internal".to_owned(),
        "https://service.localhost".to_owned(),
    ] {
        let resolved = resolve_declarations(&[ConfigDocument {
            directory: root.clone(),
            source: "explicit-fixture".into(),
            origin: PluginOrigin::LaunchExplicit,
            value: serde_json::json!({"plugins":[{"package":"./fixture.mjs","permissions":{"network":[blocked]}}]}),
        }])?;
        assert!(resolved.active.is_empty(), "{blocked} was admitted");
        assert!(
            resolved
                .blocked
                .iter()
                .any(|entry| entry.reason.contains("plugin_network_blocked:")),
            "expected plugin_network_blocked for {blocked}, got {:?}",
            resolved.blocked
        );
    }
    Ok(())
}
