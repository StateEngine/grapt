//! Pending permission truth follows the real server across stream reloads.

use super::*;
use grapt_client::wire::PermissionWireId;

const TARGET: &str = "permission-reload.txt";
const ORIGINAL: &str = "The interrupted overwrite must not run.\n";

fn overwrite_provider(stream: &mut TcpStream, _prompt: &str) {
    let arguments = serde_json::json!({"path": TARGET, "content": "replaced\n"});
    let events = [
        serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_overwrite","call_id":"call_overwrite","name":"write"}}),
        serde_json::json!({"type":"response.function_call_arguments.delta","item_id":"item_overwrite","delta":arguments.to_string()}),
        serde_json::json!({"type":"response.function_call_arguments.done","item_id":"item_overwrite"}),
        serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":11,"output_tokens":4,"total_tokens":15}}}),
    ];
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .unwrap_or_else(|error| panic!("overwrite provider response failed: {error}"));
}

pub(super) fn asking_product(label: &str) -> (Product, Client<HttpConnection>, PermissionWireId) {
    let (product, address) = serve_configured(label, true, overwrite_provider, |root| {
        let path = root.join("service.conf");
        let mut config = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("attended overwrite config: {error}"));
        config.push_str("permission_preset=author\n");
        fs::write(path, config)
            .unwrap_or_else(|error| panic!("attended overwrite config: {error}"));
    });
    fs::write(product.launch.join(TARGET), ORIGINAL)
        .unwrap_or_else(|error| panic!("overwrite target fixture failed: {error}"));
    let mut client = connect(address, Bounds::PRODUCTION);
    let _ = select_offer(&mut client, &product);
    client
        .create_session()
        .unwrap_or_else(|error| panic!("overwrite session failed: {error}"));
    client.peer_mut().resubscribe();
    assert!(client.peer_mut().subscribed(), "overwrite approver absent");
    client
        .submit_prompt("overwrite the existing file")
        .unwrap_or_else(|error| panic!("overwrite submission failed: {error}"));
    for _ in 0..JOURNEY_POLLS {
        client
            .refresh_permissions()
            .unwrap_or_else(|error| panic!("pending overwrite list failed: {error}"));
        if let Some(ask) = client.permissions().first() {
            let permission = ask.request_id.clone();
            assert!(ask.subtitle.contains("fs.overwrite"), "wrong ask: {ask:?}");
            assert!(client.peer_mut().subscribed(), "pending ask stream absent");
            return (product, client, permission);
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    panic!("the real overwrite did not ask within the bounded journey");
}

/// Read the actual API without updating the client's cached permission model.
pub(super) fn server_permissions(client: &mut Client<HttpConnection>) -> Vec<PermissionWireId> {
    let request = WireRequest::new(Operation::PermissionRequestList, None, None)
        .unwrap_or_else(|error| panic!("pending list request failed: {error}"));
    let response = client
        .peer_mut()
        .call(&request)
        .unwrap_or_else(|error| panic!("pending list transport failed: {error}"));
    assert_eq!(response.status, 200, "pending list API refused");
    grapt_client::wire::decode_permissions(&response.body)
        .unwrap_or_else(|error| panic!("pending list DTO failed: {error}"))
        .into_iter()
        .map(|row| row.id)
        .collect()
}

#[test]
fn an_interrupted_overwrite_leaves_no_ask_after_the_real_stream_reload() {
    let (product, mut client, permission) = asking_product("permission-interrupt");
    client
        .interrupt()
        .unwrap_or_else(|error| panic!("overwrite interruption failed: {error}"));
    let mut absent = false;
    for _ in 0..JOURNEY_POLLS {
        if server_permissions(&mut client).is_empty() {
            absent = true;
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(absent, "the API still lists the interrupted overwrite ask");
    assert_eq!(
        client.permissions()[0].request_id,
        permission,
        "the witness must begin with a stale client pane"
    );
    let before = client.diagnostics().reconciliations;
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("interrupted stream reload failed: {error}"));
        if client.diagnostics().reconciliations > before {
            break;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        client.diagnostics().reconciliations > before,
        "the real permission cancellation did not trigger a reload"
    );
    assert!(
        client.permissions().is_empty(),
        "the TUI retained an interrupted ask after reloading durable truth"
    );
    assert!(server_permissions(&mut client).is_empty());
    assert_eq!(
        fs::read_to_string(product.launch.join(TARGET))
            .unwrap_or_else(|error| panic!("overwrite target evidence failed: {error}")),
        ORIGINAL,
        "the interrupted overwrite executed"
    );
}

#[test]
fn a_still_pending_overwrite_survives_a_real_reconnect() {
    let (_product, mut client, permission) = asking_product("permission-reconnect");
    assert_eq!(server_permissions(&mut client), vec![permission.clone()]);
    // A new socket forces the ordinary reload rather than carrying the old
    // client's cached ask through an otherwise successful reconciliation.
    let address = client.peer_mut().address();
    let mut reconnected = connect(address, Bounds::PRODUCTION);
    reconnected.peer_mut().resubscribe();
    assert!(
        reconnected.peer_mut().subscribed(),
        "replacement approver absent"
    );
    let session = client
        .selected()
        .cloned()
        .unwrap_or_else(|| panic!("the pending session must be selected"));
    reconnected
        .select(&session)
        .unwrap_or_else(|error| panic!("pending permission reconnect failed: {error}"));
    prove_positions(&mut reconnected);
    assert_eq!(reconnected.permissions().len(), 1);
    assert_eq!(reconnected.permissions()[0].request_id, permission);
    assert_eq!(server_permissions(&mut reconnected), vec![permission]);
}
