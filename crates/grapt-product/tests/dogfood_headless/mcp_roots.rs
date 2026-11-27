//! Exercise roots through real Product HTTP selection and stdio MCP servers.
use super::*;

const PEER: &str = r#"
import json,sys,time,threading
from pathlib import Path
base=Path(sys.argv[1]+'-'+Path.cwd().name); asking=sys.argv[2]=='yes'; ordinal=0
output=threading.RLock()
def record(suffix,value):
    if suffix!='.starts' and instance>1: suffix+='-'+str(instance)
    path=base.with_suffix(suffix)
    rows=json.loads(path.read_text()) if path.exists() else []
    if len(rows)>=128: raise ValueError('fixture record bound')
    temporary=base.with_suffix(suffix+'.tmp')
    temporary.write_text(json.dumps(rows+[value]),encoding='utf-8')
    temporary.replace(path)
def send(value):
    with output: print(json.dumps(dict(jsonrpc='2.0',**value)),flush=True)
def roots():
    global ordinal
    with output:
        ordinal+=1
        send(dict(id='roots-'+str(ordinal),method='roots/list'))
instance=len(json.loads(base.with_suffix('.starts').read_text()))+1 if base.with_suffix('.starts').exists() else 1
record('.starts',True)
gate=base.with_suffix('.gate'); held=gate.exists() and instance==3
def probe():
    trigger=base.with_suffix('.probe-'+str(instance))
    for _ in range(500):
        if trigger.exists():
            roots()
            return
        time.sleep(0.01)
if asking: threading.Thread(target=probe,daemon=True).start()
for _ in range(128):
    line=sys.stdin.readline(1048577)
    if not line: break
    if len(line)>1048576: raise ValueError('fixture frame bound')
    value=json.loads(line); method=value.get('method'); ident=value.get('id')
    if method: record('.methods',method)
    if method=='initialize':
        if value['params']['capabilities'].get('roots',{}).get('listChanged') is not True:
            raise ValueError('roots capability absent')
        send(dict(id=ident,result=dict(protocolVersion=value['params']['protocolVersion'],serverInfo=dict(name='roots-fixture',version='1'),capabilities=dict(tools={}))))
    elif method=='notifications/initialized':
        if asking and not held: roots()
    elif method=='tools/list':
        if held:
            for tick in range(500):
                if not gate.exists(): break
                time.sleep(0.01)
            else: raise ValueError('startup gate not released')
            roots()
        send(dict(id=ident,result=dict(tools=[dict(name='echo',description='Echo',inputSchema=dict(type='object'))])))
    elif method=='notifications/roots/list_changed':
        if asking: roots()
    elif isinstance(ident,str) and ident.startswith('roots-'):
        result=value.get('result',value.get('error')); result['peer']=instance
        record('.roots',result)
"#;

fn observed(
    base: &Path,
    suffix: &str,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let path = base.with_extension(suffix);
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(65_537).read_to_string(&mut text)?;
    if text.len() > 65_536 {
        return Err("roots fixture output exceeded its bound".into());
    }
    serde_json::from_str(&text).map_err(Into::into)
}

fn roots_after(base: &Path, count: usize) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    response_after(base, "roots", count)
}

fn response_after(
    base: &Path,
    suffix: &str,
    count: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let wait = PollDeadline::within(Duration::from_secs(5));
    loop {
        let rows = observed(base, suffix)?;
        if rows.len() >= count {
            return rows
                .last()
                .cloned()
                .ok_or_else(|| "missing roots result".into());
        }
        if !wait.pending() {
            return Err("MCP server did not receive its roots response".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn uris(value: &serde_json::Value) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    value["roots"]
        .as_array()
        .ok_or("root list is missing")?
        .iter()
        .map(|root| {
            let uri = root["uri"].as_str().ok_or("root URI is missing")?;
            if !uri.starts_with("file://") {
                return Err("root is not a file URI".into());
            }
            Ok(uri.to_owned())
        })
        .collect()
}

fn uri(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    url::Url::from_file_path(path)
        .map(|uri| uri.to_string())
        .map_err(|()| "fixture path has no file URI".into())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn roots_follow_approved_scope_changes_without_probing_quiet_servers(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-roots");
    prepare(&root, 120_000);
    let config = root.join("global-config");
    let project = root.join("project");
    let extra = root.join("declared space # one");
    let replacement = root.join("declared two");
    let private = root.join("not-user-granted");
    for path in [&config, &project, &extra, &replacement, &private] {
        fs::create_dir_all(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    }
    let peer = |name: &str, asks: bool| {
        serde_json::json!({
            "type":"local", "command":["python3","-c",PEER,root.join(name),if asks {"yes"} else {"no"}]
        })
    };
    let mut user = serde_json::json!({"mcpRoots":[extra], "mcp":{
        "asking":peer("asking",true), "quiet":peer("quiet",false)
    }});
    let user_path = config.join("grapt.jsonc");
    fs::write(&user_path, serde_json::to_vec(&user)?)?;
    fs::write(
        project.join("grapt.jsonc"),
        serde_json::to_vec(&serde_json::json!({
            "mcpRoots":[private], "mcp":{"blocked":peer("blocked",true)}
        }))?,
    )?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let location = format!(
        "location[directory]={}",
        super::mcp_configuration_list::encode_path(&project.to_string_lossy())
    );
    let route = format!("/api/mcp/inspect?{location}");
    let first_catalog = json(address, "GET", &route, "");
    let first = uris(&roots_after(&root.join("asking-project"), 1)?)?;
    assert_eq!(first, vec![uri(&project)?, uri(&extra)?]);
    assert!(
        !root.join("blocked-project.starts").exists(),
        "unapproved server started"
    );
    user["mcpRoots"] = serde_json::json!([replacement]);
    fs::write(&user_path, serde_json::to_vec(&user)?)?;
    let refreshed = json(address, "GET", &route, "");
    assert_eq!(
        first_catalog["data"], refreshed["data"],
        "root changes altered the tool/server catalog"
    );
    assert_eq!(
        uris(&roots_after(&root.join("asking-project"), 2)?)?,
        vec![uri(&project)?, uri(&replacement)?]
    );
    assert!(observed(&root.join("quiet-project"), "roots")?.is_empty());
    assert!(!observed(&root.join("quiet-project"), "methods")?
        .iter()
        .any(|method| method == "notifications/roots/list_changed"));
    assert_eq!(
        observed(&root.join("asking-project"), "starts")?.len(),
        1,
        "root change reconnected the server"
    );
    let preview = json(
        address,
        "GET",
        &format!("/api/mcp/blocked/approval?{location}"),
        "",
    );
    let control = format!("/api/mcp/blocked/configuration?{location}");
    let approve =
        serde_json::json!({"action":"approve_project","fingerprint":preview["fingerprint"]});
    assert_eq!(
        request(address, "POST", &control, &approve.to_string()).0,
        204
    );
    json(address, "GET", &route, "");
    assert_eq!(
        uris(&roots_after(&root.join("blocked-project"), 1)?)?,
        vec![uri(&project)?, uri(&replacement)?]
    );
    // Two private Session scopes share this workspace approval. Start a third
    // with its catalog deliberately blocked, then revoke before it asks roots.
    let private_scope = || {
        let session = stateengine_binding::create_native(address, &project);
        super::bound_session_tools::bind_config(address, &root, &session);
        session
    };
    let retained = private_scope();
    json(
        address,
        "GET",
        &format!("/api/mcp/inspect?session={retained}"),
        "",
    );
    assert_eq!(
        uris(&response_after(
            &root.join("blocked-project"),
            "roots-2",
            1
        )?)?,
        vec![uri(&project)?, uri(&replacement)?],
        "approved private Session scope lost its roots"
    );
    let starting = private_scope();
    let gate = root.join("blocked-project.gate");
    fs::write(&gate, b"hold startup")?;
    let startup = thread::spawn(move || {
        json(
            address,
            "GET",
            &format!("/api/mcp/inspect?session={starting}"),
            "",
        )
    });
    let wait = PollDeadline::within(Duration::from_secs(5));
    while observed(&root.join("blocked-project"), "starts")?.len() < 3 {
        if !wait.pending() {
            return Err("private scope startup did not begin".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        request(
            address,
            "POST",
            &control,
            r#"{"action":"revoke_project_approval"}"#
        )
        .0,
        204
    );
    fs::remove_file(gate)?;
    startup
        .join()
        .map_err(|_| "startup HTTP request panicked")?;
    fs::write(
        root.join("blocked-project.probe-3"),
        b"probe installed roots",
    )?;
    for (suffix, count) in [("roots", 2), ("roots-2", 2), ("roots-3", 2)] {
        let response = response_after(&root.join("blocked-project"), suffix, count)?;
        assert!(
            uris(&response)?.is_empty(),
            "revoked scope retained roots: {response}"
        );
    }
    let second = root.join("second");
    fs::create_dir_all(&second)?;
    let created = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({
            "location":{"directory":project.join("..").join("second")}
        })
        .to_string(),
    );
    let session = created["data"]["id"]
        .as_str()
        .ok_or("new Session ID missing")?;
    json(
        address,
        "GET",
        &format!("/api/mcp/inspect?session={session}"),
        "",
    );
    assert_eq!(
        uris(&roots_after(&root.join("asking-second"), 1)?)?,
        vec![uri(&second)?, uri(&replacement)?]
    );
    assert_eq!(
        uris(&roots_after(&root.join("asking-project"), 2)?)?,
        vec![uri(&project)?, uri(&replacement)?],
        "new Session repointed the original scope"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
