//! Real MCP messages exercise resource interests through the public HTTP seam.
use super::*;

const PEER: &str = r#"
import json,sys,time,threading
from pathlib import Path
base=Path(sys.argv[1]); output=threading.RLock(); stop=threading.Event()
def send(value):
    with output: print(json.dumps(dict(jsonrpc='2.0',**value)),flush=True)
def updates():
    for _ in range(3000):
        if stop.wait(.01): return
        trigger=base.with_suffix('.update')
        if trigger.exists():
            trigger.unlink()
            send(dict(method='notifications/resources/updated',params=dict(uri='memo://guide')))
threading.Thread(target=updates,daemon=True).start()
for _ in range(256):
    line=sys.stdin.readline(1048577)
    if not line: break
    if len(line)>1048576: raise ValueError('fixture frame bound')
    value=json.loads(line); method=value.get('method'); ident=value.get('id')
    with base.with_suffix('.jsonl').open('a',encoding='utf8') as log: log.write(json.dumps(value)+'\n')
    if method=='initialize':
        send(dict(id=ident,result=dict(protocolVersion=value['params']['protocolVersion'],serverInfo=dict(name='resource-fixture'),capabilities=dict(resources=dict(subscribe=sys.argv[2]=='yes')))))
    elif method=='resources/list': send(dict(id=ident,result=dict(resources=[dict(name='guide',uri='memo://guide')])))
    elif method=='resources/templates/list': send(dict(id=ident,result=dict(resourceTemplates=[])))
    elif method=='resources/subscribe' and value['params']['uri']=='memo://refused': send(dict(id=ident,error=dict(code=-32602,message='Resource cannot be subscribed')))
    elif method in ['resources/subscribe','resources/unsubscribe']: send(dict(id=ident,result={} ))
    elif method=='resources/read': send(dict(id=ident,result=dict(contents=[dict(uri=value['params']['uri'],text='explicit resource content')])))
stop.set()
"#;

fn messages(base: &Path) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let mut text = String::new();
    fs::File::open(base.with_extension("jsonl"))?
        .take(262_145)
        .read_to_string(&mut text)?;
    if text.len() > 262_144 {
        return Err("MCP subscription fixture output exceeded its bound".into());
    }
    text.lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}
fn wait_method(base: &Path, method: &str, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let wait = PollDeadline::within(Duration::from_secs(5));
    loop {
        if messages(base)?
            .iter()
            .filter(|row| row["method"] == method)
            .count()
            >= count
        {
            return Ok(());
        }
        if !wait.pending() {
            return Err(format!("MCP peer did not receive {method}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn resource_updates_need_explicit_reads_and_interests_end_with_their_session(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-subscriptions");
    prepare(&root, 120_000);
    let config = root.join("global-config");
    fs::create_dir_all(&config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    }
    let peer = root.join("peer");
    let quiet = root.join("unsupported");
    fs::write(
        config.join("grapt.jsonc"),
        serde_json::to_vec(&serde_json::json!({"mcp":{
            "book":{"type":"local","command":["python3","-c",PEER,peer,"yes"]},
            "quiet":{"type":"local","command":["python3","-c",PEER,quiet,"no"]}
        }}))?,
    )?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let a = stateengine_binding::create_native(address, &root);
    let b = stateengine_binding::create_native(address, &root);
    let inspect = |session: &str| {
        json(
            address,
            "GET",
            &format!("/api/mcp/inspect?session={session}"),
            "",
        )
    };
    let pin = |server: &str, session: &str, uri: &str, pinned: bool| {
        request(
            address,
            "POST",
            &format!("/api/mcp/{server}/content?session={session}"),
            &serde_json::json!({"action":"pin_resource","uri":uri,"pinned":pinned}).to_string(),
        )
    };
    let read = |session: &str| {
        json(
            address,
            "POST",
            &format!("/api/mcp/book/content?session={session}"),
            r#"{"action":"read_resource","uri":"memo://guide"}"#,
        )
    };
    assert!(inspect(&a)["subscriptions"]
        .as_array()
        .ok_or("subscriptions absent")?
        .is_empty());
    assert_eq!(pin("book", &a, "memo://guide", true).0, 200);
    assert_eq!(pin("book", &b, "memo://guide", true).0, 200);
    assert_eq!(pin("book", &a, "memo://refused", true).0, 400);
    for session in [&a, &b] {
        let rows = inspect(session)["subscriptions"].clone();
        assert_eq!(rows.as_array().ok_or("subscriptions absent")?.len(), 1);
        assert_eq!(rows[0]["uri"], "memo://guide");
    }
    wait_method(&peer, "resources/subscribe", 1)?;
    let history = json(address, "GET", &format!("/api/session/{a}/message"), "");
    fs::write(peer.with_extension("update"), b"change")?;
    let wait = PollDeadline::within(Duration::from_secs(5));
    loop {
        if inspect(&a)["subscriptions"][0]["updated"] == true {
            break;
        }
        if !wait.pending() {
            return Err("subscribed resource did not become updated".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(inspect(&b)["subscriptions"][0]["updated"], true);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        json(address, "GET", &format!("/api/session/{a}/message"), ""),
        history,
        "resource notification changed conversation history"
    );
    assert!(!messages(&peer)?
        .iter()
        .any(|row| row["method"] == "resources/read"));
    let content = read(&a);
    assert!(content["content"]["text"]
        .as_str()
        .ok_or("resource text absent")?
        .contains("explicit resource content"));
    assert_eq!(inspect(&a)["subscriptions"][0]["updated"], false);
    assert_eq!(inspect(&b)["subscriptions"][0]["updated"], true);
    assert_eq!(
        messages(&peer)?
            .iter()
            .filter(|row| {
                row["method"] == "resources/subscribe" && row["params"]["uri"] == "memo://guide"
            })
            .count(),
        1,
        "shared Session interests duplicated the wire subscription"
    );
    assert_eq!(pin("book", &a, "memo://guide", false).0, 200);
    assert!(inspect(&a)["subscriptions"]
        .as_array()
        .ok_or("subscriptions absent")?
        .is_empty());
    assert!(
        !messages(&peer)?
            .iter()
            .any(|row| row["method"] == "resources/unsubscribe"),
        "one Session unpinned another's resource"
    );
    assert_eq!(
        request(address, "DELETE", &format!("/api/session/{b}"), "").0,
        204
    );
    wait_method(&peer, "resources/unsubscribe", 1)?;
    for ordinal in 0..32 {
        assert_eq!(pin("book", &a, &format!("memo://{ordinal}"), true).0, 200);
    }
    assert_eq!(pin("book", &a, "memo://overflow", true).0, 400);
    assert_eq!(pin("quiet", &a, "memo://guide", true).0, 400);
    let response = json(
        address,
        "POST",
        &format!("/api/mcp/quiet/content?session={a}"),
        r#"{"action":"read_resource","uri":"memo://guide"}"#,
    );
    assert_eq!(response["kind"], "user_content");
    assert!(!messages(&quiet)?
        .iter()
        .any(|row| row["method"] == "resources/subscribe"));
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_method(&peer, "resources/unsubscribe", 33)?;
    process_fixture::cleanup(&root);
    Ok(())
}
