//! The remote MCP server: the Model Context Protocol over Streamable HTTP —
//! the transport every standard MCP client speaks, so the endpoint is just an
//! URL (`http://<device>:25585/mcp`). `POST /mcp` carries one JSON-RPC 2.0
//! message and its reply; `initialize` gets an `Mcp-Session-Id` header;
//! notifications get `202 Accepted`; `GET` is refused (405, no SSE stream).
//! Each `tools/call` is forwarded to the game thread through a channel that
//! `Game::poll_control` drains (`mcp_answer` in `game/control.rs` answers
//! it). The listener binds every interface, so a client on another device of
//! the LAN reaches the phone too.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

/// The default listen port (vanilla's RCON convention, one past 25575).
pub const DEFAULT_PORT: u16 = 25585;

/// One forwarded tool call: the whole JSON-RPC `params` object as a line,
/// with the channel the game thread answers on.
pub use crate::control::Request;

pub struct McpServer {
    requests: mpsc::Receiver<Request>,
    closed: Arc<AtomicBool>,
    port: u16,
    /// The `{transport, url, port, tools}` file a client reads to connect.
    pub endpoint: PathBuf,
}

impl McpServer {
    /// The next tool call waiting for the game thread, if any.
    pub fn try_recv(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }

    /// The port the listener actually bound (an asked-for 0 picks a free one).
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        // Wake the accept loop so it sees `closed`.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

/// A random id (session ids): hash jitter, no external randomness needed.
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    const HEX: &[u8] = b"0123456789abcdef";
    let mut bits = 0u64;
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        bits = bits.rotate_left(32) ^ h.finish();
    }
    (0..32)
        .map(|i| {
            let nibble = ((bits.rotate_left(i as u32) >> 60) & 0xf) as usize;
            HEX[nibble] as char
        })
        .collect()
}

/// The tool list: what `tools/list` advertises and `tools/call` accepts.
fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "nbt_get",
            "description": "Read an entity's NBT (players, mobs and dropped item stacks included) or a block entity's NBT (command blocks, chests, signs…) as SNBT.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["entity", "block"] },
                    "target": { "type": "string", "description": "entity: a selector like @e[type=minecraft:cow,limit=1] or a player name; block: \"x y z\"" },
                    "path": { "type": "string", "description": "optional NBT path into the tag, like Inventory[0] or Pos" }
                },
                "required": ["kind", "target"]
            }
        }),
        json!({
            "name": "nbt_merge",
            "description": "Merge SNBT into an entity's or a block entity's NBT (as /data merge does): the given keys are set, others kept. Players refuse merges, as in vanilla. Use it to change a mob's equipment, a command block's Command, an item's components…",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["entity", "block"] },
                    "target": { "type": "string", "description": "entity selector/player name, or \"x y z\" for a block" },
                    "nbt": { "type": "string", "description": "SNBT to merge, like {Command:\"say hi\",auto:1b}" }
                },
                "required": ["kind", "target", "nbt"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Run a game command (with or without the leading /) as the game's own player: /give, /tp, /summon, /effect, anything the chat accepts. The output shows in the game's chat.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "build_command_chain",
            "description": "Place a row of command blocks forming a command chain: block i sits at from + i*step along axis, each facing the next, with its command merged in. Returns the block positions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "array", "items": { "type": "integer" }, "minItems": 3, "maxItems": 3 },
                    "commands": { "type": "array", "items": { "type": "string" }, "description": "the commands, in chain order, without the leading /" },
                    "kind": { "type": "string", "enum": ["impulse", "chain", "repeating"], "description": "the first block's type; the rest are chain blocks" },
                    "auto": { "type": "boolean", "description": "redstone-independent activation, default true" },
                    "conditional": { "type": "boolean", "description": "default false" },
                    "axis": { "type": "string", "enum": ["x", "z"], "description": "the row direction, default x" },
                    "step": { "type": "integer", "enum": [1, -1], "description": "default 1" }
                },
                "required": ["from", "commands"]
            }
        }),
    ]
}

/// One HTTP response ready to write: status line reason, extra headers,
/// and an optional body.
struct HttpResponse {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Option<String>,
}

impl HttpResponse {
    fn json(status: u16, value: &Value) -> Self {
        HttpResponse {
            status,
            headers: vec![("Content-Type", "application/json".into())],
            body: Some(value.to_string()),
        }
    }

    fn empty(status: u16) -> Self {
        HttpResponse {
            status,
            headers: Vec::new(),
            body: None,
        }
    }
}

/// Start listening on every interface (so other devices reach the phone),
/// write the endpoint file, and answer MCP requests by forwarding
/// `tools/call` to the game thread.
pub fn start(port: u16, endpoint: &Path) -> std::io::Result<McpServer> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let port = listener.local_addr()?.port();
    std::fs::write(
        endpoint,
        json!({
            "transport": "streamable-http",
            "url": format!("http://0.0.0.0:{port}/mcp"),
            "port": port,
            "protocol": "2025-06-18",
            "tools": tools().iter().map(|t| t["name"].clone()).collect::<Vec<_>>(),
        })
        .to_string(),
    )?;
    let (sender, requests) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let accept_closed = closed.clone();
    // Live MCP sessions, from `initialize`: kept only to validate a client's
    // `Mcp-Session-Id` header when it sends one (lenient when absent).
    let sessions: Arc<Mutex<HashMap<String, ()>>> = Arc::new(Mutex::new(HashMap::new()));
    std::thread::Builder::new()
        .name("horizon-mcp".into())
        .spawn(move || {
            while !accept_closed.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let sender = sender.clone();
                let sessions = sessions.clone();
                let closed = accept_closed.clone();
                // One thread per client; the game thread answers the calls.
                std::thread::spawn(move || serve(stream, sender, sessions, closed));
            }
        })?;
    Ok(McpServer {
        requests,
        closed,
        port,
        endpoint: endpoint.to_owned(),
    })
}

/// One keep-alive HTTP connection: parse requests, answer until EOF/close.
fn serve(
    stream: TcpStream,
    sender: mpsc::Sender<Request>,
    sessions: Arc<Mutex<HashMap<String, ()>>>,
    closed: Arc<AtomicBool>,
) {
    let _ = stream.set_nodelay(true);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        let Some(request) = read_request(&mut reader) else {
            break;
        };
        let response = handle(&request, &sender, &sessions);
        if write_response(&mut writer, &response).is_err() {
            break;
        }
        if !request.keep_alive {
            break;
        }
    }
    let _ = writer.flush();
}

/// One parsed HTTP/1.1 request: method, path, body, keep-alive wish.
struct HttpRequest {
    method: String,
    body: String,
    keep_alive: bool,
}

/// Read one request (request line, headers, content-length body) off the
/// stream; `None` on EOF or a malformed start.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<HttpRequest> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.trim().split(' ');
    let method = parts.next()?.to_owned();
    let _path = parts.next().unwrap_or("/");
    let mut content_length = 0usize;
    let mut keep_alive = true;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "content-length" => content_length = value.parse().unwrap_or(0),
            "connection" => {
                if value.eq_ignore_ascii_case("close") {
                    keep_alive = false;
                }
            }
            _ => {}
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(HttpRequest {
        method,
        body: String::from_utf8_lossy(&body).to_string(),
        keep_alive,
    })
}

/// Answer one request: route by method, then classify the JSON-RPC message —
/// a reply from the server itself, a notification (202), or a tool call the
/// game thread has to answer.
fn handle(
    request: &HttpRequest,
    sender: &mpsc::Sender<Request>,
    sessions: &Arc<Mutex<HashMap<String, ()>>>,
) -> HttpResponse {
    // GET (no SSE stream) and other methods are refused; DELETE ends a
    // session; only POST carries JSON-RPC.
    let refused = match request.method.as_str() {
        "GET" => Some(HttpResponse {
            status: 405,
            headers: vec![("Allow", "POST".into())],
            body: None,
        }),
        // Ending a session: accepted and forgotten.
        "DELETE" => Some(HttpResponse::empty(204)),
        m if m != "POST" => Some(HttpResponse::empty(405)),
        _ => None,
    };
    if let Some(response) = refused {
        return response;
    }
    let msg: Value = match serde_json::from_str(&request.body) {
        Ok(msg) => msg,
        Err(e) => {
            return HttpResponse::json(
                200,
                &json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32700, "message": e.to_string() }
                }),
            );
        }
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let id = msg.get("id").cloned();
    let mut forward = false;
    let result: std::result::Result<Value, (i64, String)> = match method {
        "initialize" => {
            let session = random_token();
            let _ = sessions.lock().map(|mut s| s.insert(session.clone(), ()));
            let mut response = json!({
                "protocolVersion": msg.get("params").and_then(|p| p.get("protocolVersion")).and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "horizon-android-mcp", "version": horizon_core::release::VERSION },
                "instructions": "Remote control of a running Horizon (a Rust Minecraft Java Edition reimplementation). Read and merge NBT of entities (players, mobs, item stacks) and block entities with nbt_get/nbt_merge, run any game command with run_command, and lay out command-block chains with build_command_chain.",
            });
            if id.is_none() {
                // A malformed initialize without an id still needs a reply.
                response = json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32600, "message": "initialize requires an id" }
                });
                return HttpResponse::json(200, &response);
            }
            let mut http = HttpResponse::json(200, &json!({
                "jsonrpc": "2.0", "id": id.clone().unwrap(),
                "result": response,
            }));
            http.headers
                .push(("Mcp-Session-Id", session));
            return http;
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            forward = true;
            Ok(json!({}))
        }
        // `notifications/initialized` and friends: nothing to say back.
        _ if id.is_none() => return HttpResponse::empty(202),
        _ => Err((-32601, format!("Method not found: {method}"))),
    };
    if forward {
        return forward_call(&msg, sender);
    }
    HttpResponse::json(
        200,
        &match (id, result) {
            (Some(id), Ok(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            (Some(id), Err((code, message))) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message }
            }),
            (None, _) => json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32600, "message": "request without an id" }
            }),
        },
    )
}

/// Write the response: status line, headers, content length, body.
fn write_response(writer: &mut TcpStream, response: &HttpResponse) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = response.body.as_deref().unwrap_or("");
    write!(
        writer,
        "{head}Content-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    )?;
    if !body.is_empty() {
        writer.write_all(body.as_bytes())?;
    }
    writer.flush()
}

/// One `tools/call` to the game thread: its `params` go over the channel as
/// a line, the JSON reply becomes the MCP result (or error).
fn forward_call(msg: &Value, sender: &mpsc::Sender<Request>) -> HttpResponse {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !tools().iter().any(|t| t["name"].as_str() == Some(name.as_str())) {
        return HttpResponse::json(
            200,
            &json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32602, "message": format!("Unknown tool {name}") }
            }),
        );
    }
    let (reply, response) = mpsc::channel();
    if sender
        .send(Request {
            line: params.to_string(),
            reply,
        })
        .is_err()
    {
        return HttpResponse::json(
            200,
            &json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "game exited" }
            }),
        );
    }
    let text = response
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap_or_else(|_| {
            r#"{"error":"the game thread did not answer (paused or gone)"}"#.to_owned()
        });
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }));
    match value.get("error") {
        Some(error) => HttpResponse::json(
            200,
            &json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": error.clone() }
            }),
        ),
        None => HttpResponse::json(
            200,
            &json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                    "isError": false
                }
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};

    /// A POST to the live server; returns (status, headers, body).
    fn post(
        stream: &mut TcpStream,
        body: &str,
        session: Option<&str>,
        method: &str,
    ) -> (u16, HashMap<String, String>, String) {
        let mut head = format!("{method} /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        if let Some(session) = session {
            head.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
        }
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.flush().unwrap();
        read_response(stream)
    }

    fn read_response(
        stream: &mut TcpStream,
    ) -> (u16, HashMap<String, String>, String) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status: u16 = line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut headers = HashMap::new();
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim().to_owned();
                if name == "content-length" {
                    length = value.parse().unwrap_or(0);
                }
                headers.insert(name, value);
            }
        }
        let mut body = vec![0u8; length];
        if length > 0 {
            reader.read_exact(&mut body).unwrap();
        }
        (status, headers, String::from_utf8_lossy(&body).to_string())
    }

    #[test]
    fn http_initialize_tools_and_unknown_tool() {
        let dir =
            std::env::temp_dir().join(format!("horizon-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = dir.join("mcp-endpoint.json");
        let server = start(0, &endpoint).unwrap();
        let mut stream =
            TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        // initialize: 200, a session id header, and the protocol reply.
        let (status, headers, body) = post(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
            None,
            "POST",
        );
        assert_eq!(status, 200, "{body}");
        assert!(headers.contains_key("mcp-session-id"));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v["result"]["protocolVersion"].is_string());
        let session = headers["mcp-session-id"].clone();
        // The same connection keeps speaking (keep-alive): tools/list.
        let (status, _, body) = post(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            Some(&session),
            "POST",
        );
        assert_eq!(status, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for want in ["nbt_get", "nbt_merge", "run_command", "build_command_chain"] {
            assert!(names.contains(&want), "{want}");
        }
        // An unknown tool errors without reaching the game thread.
        let (_, _, body) = post(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
            Some(&session),
            "POST",
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["id"], 3);
        assert!(v["error"]["message"].as_str().unwrap().contains("nope"));
        // A notification (no id) is accepted with no body.
        let (status, _, body) = post(
            &mut stream,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            Some(&session),
            "POST",
        );
        assert_eq!(status, 202);
        assert!(body.is_empty());
        // GET is refused (no SSE stream), DELETE ends the session.
        let (status, headers, _) = post(
            &mut stream,
            "",
            Some(&session),
            "GET",
        );
        assert_eq!(status, 405);
        assert_eq!(headers.get("allow").map(String::as_str), Some("POST"));
        let (status, _, _) = post(&mut stream, "", Some(&session), "DELETE");
        assert_eq!(status, 204);
        // The endpoint file points clients at the /mcp URL.
        let ep: Value =
            serde_json::from_str(&std::fs::read_to_string(&endpoint).unwrap()).unwrap();
        assert_eq!(ep["transport"], "streamable-http");
        assert_eq!(
            ep["url"],
            format!("http://0.0.0.0:{}/mcp", server.port())
        );
        drop(server);
        assert!(!endpoint.exists());
    }

    #[test]
    fn ping_and_parse_error() {
        let dir =
            std::env::temp_dir().join(format!("horizon-mcp-ping-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let endpoint = dir.join("mcp-endpoint.json");
        let server = start(0, &endpoint).unwrap();
        let mut stream =
            TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        let (status, _, body) = post(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
            None,
            "POST",
        );
        assert_eq!(status, 200);
        assert!(body.contains("\"result\":{}"), "{body}");
        // Unparsable JSON: a JSON-RPC parse error, still over HTTP 200.
        let (_, _, body) = post(&mut stream, "not json", None, "POST");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }
}
