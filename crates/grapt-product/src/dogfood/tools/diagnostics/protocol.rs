//! Bounded, side-effect-free LSP diagnostics exchange for one contained server.

use std::path::Path;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};
use url::Url;

const MESSAGE_LIMIT: usize = 1024 * 1024;
const INPUT_LIMIT: usize = 4 * MESSAGE_LIMIT;
const OUTPUT_LIMIT: usize = 64 * 1024;

pub(super) async fn collect(
    stdin: &mut ChildStdin,
    stdout: ChildStdout,
    root: &Path,
    initialization: &Value,
    documents: &[super::Document],
) -> Result<Vec<Value>, String> {
    let root_uri = Url::from_directory_path(root)
        .map_err(|()| "LSP workspace root must be an absolute directory path".to_owned())?;
    let folders = json!([{"uri": root_uri.as_str(), "name": "workspace"}]);
    let mut reader = Reader {
        stdout: BufReader::new(stdout),
        bytes: 0,
        frames: 0,
    };
    let initialize = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
        "processId":null, "rootUri":root_uri.as_str(), "workspaceFolders":folders,
        "initializationOptions":initialization,
        "capabilities":{"workspace":{"configuration":true, "applyEdit":false},
            "textDocument":{"publishDiagnostics":{"versionSupport":true}}}
    }});
    let (replies, pending_replies) = mpsc::channel(32);
    let (shutdown_written, shutdown_pending) = oneshot::channel();
    let (results, ()) = tokio::try_join!(
        read_exchange(
            &mut reader,
            replies,
            shutdown_pending,
            initialization,
            &folders,
            documents
        ),
        write_exchange(
            stdin,
            pending_replies,
            shutdown_written,
            initialize,
            documents
        ),
    )?;
    let output = serde_json::to_vec(&results).map_err(|error| error.to_string())?;
    if output.len() > OUTPUT_LIMIT {
        return Err("LSP diagnostics exceed the 64 KiB output limit".to_owned());
    }
    Ok(results)
}

enum Outbound {
    Reply(String),
    Initialized { save_text: bool },
    DiagnosticsComplete,
}

async fn write_exchange(
    stdin: &mut ChildStdin,
    mut replies: mpsc::Receiver<Outbound>,
    shutdown_written: oneshot::Sender<()>,
    initialize: Value,
    documents: &[super::Document],
) -> Result<(), String> {
    send(stdin, initialize).await?;
    let save_text = loop {
        match replies
            .recv()
            .await
            .ok_or("LSP reader ended before initialization")?
        {
            Outbound::Reply(reply) => write_frame(stdin, &reply).await?,
            Outbound::Initialized { save_text } => break save_text,
            Outbound::DiagnosticsComplete => {
                return Err("LSP diagnostics preceded initialization".to_owned())
            }
        }
    };
    let notifications = std::iter::once(json!({
        "jsonrpc":"2.0", "method":"initialized", "params":{}
    }))
    .chain(documents.iter().flat_map(|document| {
        let mut params = json!({"textDocument":{"uri":document.uri.as_str()}});
        if save_text {
            params["text"] = Value::String(document.text.clone());
        }
        [
            json!({"jsonrpc":"2.0", "method":"textDocument/didOpen", "params":{
                "textDocument":{"uri":document.uri.as_str(), "languageId":document.language,
                    "version":document.version.0, "text":document.text}
            }}),
            json!({"jsonrpc":"2.0", "method":"textDocument/didSave", "params":params}),
        ]
    }));
    let mut complete = false;
    for notification in notifications {
        loop {
            match replies.try_recv() {
                Ok(Outbound::Reply(reply)) => write_frame(stdin, &reply).await?,
                Ok(Outbound::DiagnosticsComplete) => complete = true,
                Ok(Outbound::Initialized { .. }) => {
                    return Err("duplicate LSP initialization".to_owned())
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(
                        "LSP reader ended before document notifications completed".to_owned()
                    );
                }
            }
        }
        // Never interrupt a partially written frame to service another message.
        send(stdin, notification).await?;
    }
    while !complete {
        match replies
            .recv()
            .await
            .ok_or("LSP reader ended before diagnostics completed")?
        {
            Outbound::Reply(reply) => write_frame(stdin, &reply).await?,
            Outbound::DiagnosticsComplete => complete = true,
            Outbound::Initialized { .. } => return Err("duplicate LSP initialization".to_owned()),
        }
    }
    send(
        stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"shutdown", "params":null}),
    )
    .await?;
    shutdown_written
        .send(())
        .map_err(|()| "LSP reader ended before shutdown".to_owned())?;
    while let Some(command) = replies.recv().await {
        let Outbound::Reply(reply) = command else {
            return Err("unexpected LSP lifecycle notification after shutdown".to_owned());
        };
        write_frame(stdin, &reply).await?;
    }
    send(stdin, json!({"jsonrpc":"2.0", "method":"exit"})).await
}

async fn read_exchange(
    reader: &mut Reader,
    replies: mpsc::Sender<Outbound>,
    shutdown_pending: oneshot::Receiver<()>,
    initialization: &Value,
    folders: &Value,
    documents: &[super::Document],
) -> Result<Vec<Value>, String> {
    let mut results = vec![None; documents.len()];
    let mut initialized = false;
    let mut complete = false;
    loop {
        if initialized && !complete && results.iter().all(Option::is_some) {
            replies
                .try_send(Outbound::DiagnosticsComplete)
                .map_err(|error| format!("LSP reply queue unavailable: {error}"))?;
            complete = true;
        }
        let message = reader
            .read()
            .await
            .map_err(|error| format!("LSP diagnostics exchange is incomplete: {error}"))?;
        if let Some(reply) = request(&message, initialization, folders)? {
            replies
                .try_send(Outbound::Reply(frame(reply)?))
                .map_err(|error| format!("LSP reply queue unavailable: {error}"))?;
            continue;
        }
        if !initialized {
            if message.get("method").is_some() {
                continue;
            }
            let result = response_result(&message, 1)?;
            let capabilities = result
                .get("capabilities")
                .filter(|value| value.is_object())
                .ok_or("LSP initialize result must contain a capabilities object")?;
            let save_text = capabilities.pointer("/textDocumentSync/save/includeText")
                == Some(&Value::Bool(true));
            replies
                .try_send(Outbound::Initialized { save_text })
                .map_err(|error| format!("LSP reply queue unavailable: {error}"))?;
            initialized = true;
            continue;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            if !complete {
                return Err("unexpected LSP response while awaiting diagnostics".to_owned());
            }
            if !response_result(&message, 2)?.is_null() {
                return Err("LSP shutdown result must be null".to_owned());
            }
            shutdown_pending
                .await
                .map_err(|error| format!("LSP shutdown write did not complete: {error}"))?;
            return results
                .into_iter()
                .map(|result| {
                    result.ok_or_else(|| "missing LSP diagnostics notification".to_owned())
                })
                .collect();
        };
        if complete || method != "textDocument/publishDiagnostics" {
            continue;
        }
        let (uri, version, items) = publication(&message)?;
        let Some(index) = documents.iter().position(|document| document.uri == uri) else {
            continue;
        };
        if version.is_some_and(|version| version != documents[index].version.0) {
            continue;
        }
        let path = documents[index]
            .path
            .to_str()
            .ok_or("LSP document path must be UTF-8")?;
        results[index] = Some(json!({
            "path":path, "status":if items.is_empty() { "clean" } else { "reported" },
            "version":version, "versioned":version.is_some(), "items":items
        }));
    }
}

fn frame(message: Value) -> Result<String, String> {
    let body = serde_json::to_string(&message).map_err(|error| error.to_string())?;
    if body.len() > MESSAGE_LIMIT {
        return Err("outgoing LSP message exceeds 1 MiB".to_owned());
    }
    Ok(format!("Content-Length: {}\r\n\r\n{body}", body.len()))
}

async fn send(stdin: &mut ChildStdin, message: Value) -> Result<(), String> {
    write_frame(stdin, &frame(message)?).await
}

async fn write_frame(stdin: &mut ChildStdin, frame: &str) -> Result<(), String> {
    stdin
        .write_all(frame.as_bytes())
        .await
        .map_err(|error| format!("write LSP frame: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("flush LSP input: {error}"))
}

struct Reader {
    stdout: BufReader<ChildStdout>,
    bytes: usize,
    frames: usize,
}

impl Reader {
    async fn read(&mut self) -> Result<Value, String> {
        if self.frames >= 512 {
            return Err("LSP input exceeds 512 frames".to_owned());
        }
        self.frames += 1;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() >= 4096 || self.bytes >= INPUT_LIMIT {
                return Err("LSP header exceeds 4096 bytes or input exceeds 4 MiB".to_owned());
            }
            header.push(
                self.stdout
                    .read_u8()
                    .await
                    .map_err(|error| format!("read LSP header: {error}"))?,
            );
            self.bytes += 1;
        }
        let length = content_length(&header)?;
        if length == 0 || length > MESSAGE_LIMIT || self.bytes + length > INPUT_LIMIT {
            return Err("LSP body is empty, exceeds 1 MiB, or input exceeds 4 MiB".to_owned());
        }
        let mut body = vec![0; length];
        self.stdout
            .read_exact(&mut body)
            .await
            .map_err(|error| format!("read LSP body: {error}"))?;
        self.bytes += length;
        let message: Value =
            serde_json::from_slice(&body).map_err(|error| format!("invalid LSP JSON: {error}"))?;
        envelope(&message)?;
        if message.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && message.get("id").is_none()
        {
            publication(&message)?;
        }
        Ok(message)
    }
}

fn content_length(header: &[u8]) -> Result<usize, String> {
    let header =
        std::str::from_utf8(header).map_err(|error| format!("invalid LSP header: {error}"))?;
    let mut length = None;
    for line in header[..header.len() - 4].split("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or("LSP header field is missing a colon")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err("invalid LSP header field".to_owned());
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err("duplicate or invalid LSP Content-Length".to_owned());
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|error| format!("invalid LSP length: {error}"))?,
            );
        } else if name.eq_ignore_ascii_case("Content-Type") {
            for parameter in value.split(';').skip(1) {
                let (key, encoding) = parameter
                    .trim()
                    .split_once('=')
                    .ok_or("invalid LSP Content-Type parameter")?;
                if key.trim().eq_ignore_ascii_case("charset")
                    && !encoding.trim().eq_ignore_ascii_case("utf-8")
                    && !encoding.trim().eq_ignore_ascii_case("utf8")
                {
                    return Err("LSP Content-Type must use UTF-8".to_owned());
                }
            }
        }
    }
    length.ok_or_else(|| "LSP header is missing Content-Length".to_owned())
}

fn envelope(message: &Value) -> Result<(), String> {
    let object = message.as_object().ok_or("LSP message must be an object")?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("LSP message must use JSON-RPC 2.0".to_owned());
    }
    if object
        .get("id")
        .is_some_and(|id| !id.is_string() && id.as_i64().is_none())
    {
        return Err("LSP message ID must be a string or integer".to_owned());
    }
    if let Some(method) = object.get("method") {
        if method.as_str().is_none_or(str::is_empty)
            || object.contains_key("result")
            || object.contains_key("error")
            || object.get("params").is_some_and(|params| {
                !params.is_object() && !params.is_array() && !params.is_null()
            })
        {
            return Err("malformed LSP request or notification".to_owned());
        }
    } else {
        if !object.contains_key("id")
            || object.contains_key("params")
            || object.contains_key("result") == object.contains_key("error")
        {
            return Err("malformed LSP response".to_owned());
        }
        if object.get("error").is_some_and(|error| {
            error.get("code").and_then(Value::as_i64).is_none()
                || error.get("message").and_then(Value::as_str).is_none()
        }) {
            return Err("malformed LSP error response".to_owned());
        }
    }
    Ok(())
}

fn response_result(message: &Value, id: i64) -> Result<Value, String> {
    if message.get("id").and_then(Value::as_i64) != Some(id) {
        return Err("unexpected LSP response ID".to_owned());
    }
    if let Some(error) = message.get("error") {
        let code = &error["code"];
        return Err(format!("LSP request {id} failed with code {code}"));
    }
    Ok(message["result"].clone())
}

fn request(
    message: &Value,
    initialization: &Value,
    folders: &Value,
) -> Result<Option<Value>, String> {
    let (Some(method), Some(id)) = (
        message.get("method").and_then(Value::as_str),
        message.get("id"),
    ) else {
        return Ok(None);
    };
    let result = match method {
        "workspace/configuration" => {
            let items = message
                .pointer("/params/items")
                .and_then(Value::as_array)
                .ok_or("LSP configuration request must contain an items array")?;
            if items.len() > 32 || items.iter().any(|item| !item.is_object()) {
                return Err(
                    "LSP configuration request has invalid items or exceeds 32 items".to_owned(),
                );
            }
            Value::Array(vec![initialization.clone(); items.len()])
        }
        "workspace/workspaceFolders" => folders.clone(),
        "client/registerCapability" | "window/workDoneProgress/create" => Value::Null,
        "workspace/applyEdit" => {
            json!({"applied":false, "failureReason":"Diagnostics collection cannot apply edits"})
        }
        _ => {
            return Ok(Some(json!({"jsonrpc":"2.0", "id":id,
            "error":{"code":-32601, "message":"Method not supported"}})))
        }
    };
    Ok(Some(json!({"jsonrpc":"2.0", "id":id, "result":result})))
}

fn publication(message: &Value) -> Result<(Url, Option<i32>, &[Value]), String> {
    let params = message
        .get("params")
        .ok_or("LSP diagnostics are missing params")?;
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or("LSP diagnostics URI must be a string")?;
    let uri = Url::parse(uri).map_err(|error| format!("invalid LSP diagnostics URI: {error}"))?;
    let version = params
        .get("version")
        .map(|version| {
            version
                .as_i64()
                .and_then(|version| i32::try_from(version).ok())
                .ok_or("LSP diagnostics version must be an i32 when present")
        })
        .transpose()?;
    let items = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or("LSP diagnostics must be an array")?;
    if items.len() > 128 {
        return Err("LSP diagnostics exceed 128 items per document".to_owned());
    }
    for item in items {
        if item.get("message").and_then(Value::as_str).is_none() {
            return Err("LSP diagnostic message must be a string".to_owned());
        }
        if item.get("severity").is_some_and(|severity| {
            severity
                .as_u64()
                .is_none_or(|severity| !(1..=4).contains(&severity))
        }) {
            return Err("LSP diagnostic severity must be between 1 and 4".to_owned());
        }
        let start = position(item.pointer("/range/start"))?;
        let end = position(item.pointer("/range/end"))?;
        if start > end {
            return Err("LSP diagnostic range ends before it starts".to_owned());
        }
    }
    Ok((uri, version, items.as_slice()))
}

fn position(value: Option<&Value>) -> Result<(u64, u64), String> {
    let value = value.ok_or("LSP diagnostic range is missing a position")?;
    let coordinate = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_u64)
            .filter(|value| *value <= i32::MAX as u64)
            .ok_or("LSP diagnostic position must contain unsigned 31-bit line and character")
    };
    Ok((coordinate("line")?, coordinate("character")?))
}
