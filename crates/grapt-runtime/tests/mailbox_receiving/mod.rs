//! One bounded loopback mail producer, joined even when a consumer test fails.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

pub(super) struct Peer {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<Vec<Value>, String>>>,
}
impl Peer {
    pub fn start(version: u8, kind: &str, body: &str) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let row = json!({"protocol":"remote-seat-mail-transport/v1","source_binding":"binding",
            "projects":{"source":"source","target":"target"},"source_message":"mail-one",
            "source_cursor":"cursor-one","sender":"sender","recipient":"recipient","kind":kind,"body":body,
            "refs":[],"supersession":{"supersedes":null,"superseded_by":null},
            "archive":{"disposition":"active","reason":null},"source_created":"2026-10-13T15:00:00.000Z","attention":"wake"});
        let worker = thread::spawn(move || serve(listener, &stopping, version, row));
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }
    pub fn finish(mut self) -> Result<Vec<Value>, String> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .take()
            .ok_or("mail peer missing")?
            .join()
            .map_err(|_| "mail peer panicked")?
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            assert!(
                thread::panicking() || result.is_ok_and(|value| value.is_ok()),
                "mail peer failed during cleanup"
            );
        }
    }
}

fn serve(
    listener: TcpListener,
    stop: &AtomicBool,
    version: u8,
    row: Value,
) -> Result<Vec<Value>, String> {
    let mut requests = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let (mut socket, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        let request = read(&mut socket, version)?;
        let operation = request["operation"].as_str().ok_or("operation")?;
        let value = response(&request, version, &row)?;
        let closed = operation == "close";
        requests.push(request);
        if requests.len() > 16 {
            return Err("mail peer request bound".into());
        }
        let body = serde_json::to_vec(&json!({"ok":true,"value":value}))
            .map_err(|error| error.to_string())?;
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).map_err(|error| error.to_string())?;
        socket.write_all(&body).map_err(|error| error.to_string())?;
        if closed {
            return Ok(requests);
        }
    }
    Ok(requests)
}

fn response(request: &Value, version: u8, row: &Value) -> Result<Value, String> {
    Ok(match request["operation"].as_str().ok_or("operation")? {
        "open" => {
            if version == 2 {
                let kinds = request["args"]["request"]["accepted_kinds"]
                    .as_array()
                    .ok_or("declared kinds")?;
                if !kinds.iter().any(|kind| kind == "finding") {
                    return Err("finding was not declared".into());
                }
            }
            json!({"binding_id":"binding","protocol":format!("remote-seat-mail-transport/v{version}"),
                "adapter_version":format!("stateengine-native-mail/{version}"),"projects":{"source":"source","target":"target"},
                "verified_principal":"recipient","local_recipient":"recipient","mapping_version":"exact-seat-id/v1",
                "takeover":{"generation":1,"displaced":null},"limits":request["args"]["limits"],
                "capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,
                    "explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
        }
        "subscribe_and_snapshot" => json!({"subscription_id":"subscription","generation":1,
            "snapshot":{"snapshot_id":"snapshot","barrier":"barrier","start_after":request["args"]["resume"]["from_checkpoint"],"retained_origin":"origin"}}),
        "fetch_snapshot_page" => {
            let messages = if request["args"]["after"].is_null() {
                vec![row.clone()]
            } else {
                Vec::new()
            };
            let mut page = json!({"messages":messages,"next_after":null});
            if version == 2 {
                page["refused_rows"] = json!([]);
                page["last_cursor"] = if messages.is_empty() {
                    Value::Null
                } else {
                    json!("cursor-one")
                };
            }
            page
        }
        "acknowledge_delivery" => {
            let ack = &request["args"]["ack"];
            json!({"tag":"Recorded","receipt":{"ack_id":ack["ack_id"],"source_message":ack["source_message"],"projects":ack["projects"]},
                "primary_delivery":{"witnessed":true},"floor_projection":{"state":"unarmed","outcome":"refused","disposition":null,
                    "reason":"secondary-unarmed","outcome_record":{"state":"unreadable"},"retry_required":false,"retry":null}})
        }
        "close" => json!({"joined_tasks":0,"ticks_used":1}),
        other => return Err(format!("unexpected mail operation {other}")),
    })
}

fn read(socket: &mut TcpStream, version: u8) -> Result<Value, String> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 2048];
        let count = socket.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 || bytes.len() + count > 32768 {
            return Err("mail request bound or truncation".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let head = std::str::from_utf8(&bytes[..split]).map_err(|error| error.to_string())?;
        if !head.starts_with(&format!(
            "POST /remote-seat-mail-transport/v{version} HTTP/1.1\r\n"
        )) || !head
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer MAIL_FIXTURE_CAPABILITY"))
        {
            return Err("mail route or authorization mismatch".into());
        }
        let length = head
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            })
            .ok_or("content length")?;
        let start = split + 4;
        if bytes.len() >= start + length {
            return serde_json::from_slice(&bytes[start..start + length])
                .map_err(|error| error.to_string());
        }
    }
}
