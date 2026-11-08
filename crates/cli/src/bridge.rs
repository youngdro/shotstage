//! Live bridge: lets the CLI / MCP server drive an editor open in a browser.
//!
//! `shotstage mcp --live` listens on `127.0.0.1:<port>`. A browser editor connects to
//! `ws://127.0.0.1:<port>/bridge` (the page served at `http://127.0.0.1:<port>/` connects by
//! itself; any other page can call `connectBridge(editor, url)` from `bridge.js`). While a
//! browser is connected every command runs in that editor, so the person watching sees each
//! change as it happens and snapshots come from the browser's renderer; when none is connected,
//! commands run on a local copy of the scene (which stays in sync), so nothing is lost.
//!
//! Wire format (JSON text frames): the server sends `{"id": n, "request": {...}}` with the same
//! request objects as `editor.request` (`{command, params, merge?}` or `{batch}`); the editor
//! answers `{"id": n, "ok": true, "outcome": {result, revision, changed}}` or
//! `{"id": n, "ok": false, "error": {code, message}}`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use shotstage_commands::{CmdResult, CommandError, Outcome, Session};
use shotstage_core::Scene;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

use crate::host::{Backend, Local};

/// How long one forwarded command may take in the browser.
const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Link {
    ws: Option<WebSocket<TcpStream>>,
    /// Connected, not yet synced with the local copy.
    fresh: bool,
    peer: String,
    next_id: u64,
    connections: u64,
}

pub struct Options {
    pub port: u16,
    /// Folder with the built web editor (dist/) to serve at `/`.
    pub web: Option<PathBuf>,
    /// Extra page origins allowed to connect (localhost ones always are).
    pub allow_origins: Vec<String>,
}

pub struct Bridge {
    pub local: Local,
    link: Arc<Mutex<Link>>,
    pub url: String,
    pub web: bool,
}

fn origin_ok(origin: Option<&str>, extra: &[String]) -> bool {
    let Some(o) = origin else { return true }; // not a browser page (scripts, tools)
    if extra.iter().any(|e| e == o) {
        return true;
    }
    let host = o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")).unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "css" => "text/css",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

const NO_WEB: &str = "<!doctype html><meta charset=utf-8><title>shotstage bridge</title><body style=\"font:15px system-ui;margin:40px;max-width:640px\"><h1>shotstage bridge</h1><p>The bridge is running. Start the server with <code>--web dist</code> (the folder <code>scripts/build-web.sh</code> builds) to open the editor here, or connect an editor page you already have:</p><pre>import { connectBridge } from \"./bridge.js\";\nconnectBridge(editor, \"WS_URL\");</pre>";

fn serve_http(mut stream: TcpStream, head: &str, web: Option<&Path>, ws_url: &str) {
    let path = head.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split(['?', '#']).next().unwrap_or("/");
    if path == "/bridge.json" {
        let body = json!({ "ws": ws_url }).to_string();
        return respond(&mut stream, "200 OK", "application/json", body.as_bytes());
    }
    let Some(root) = web else {
        return respond(&mut stream, "200 OK", "text/html; charset=utf-8", NO_WEB.replace("WS_URL", ws_url).as_bytes());
    };
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    if rel.split('/').any(|seg| seg == ".." || seg.starts_with('.')) {
        return respond(&mut stream, "404 Not Found", "text/plain", b"not found");
    }
    let file = root.join(rel);
    match std::fs::read(&file) {
        Ok(bytes) => respond(&mut stream, "200 OK", content_type(&file), &bytes),
        Err(_) => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

#[allow(clippy::result_large_err)] // the handshake callback type is tungstenite's
fn handle(stream: TcpStream, link: &Arc<Mutex<Link>>, opts: &Arc<Options>, ws_url: &str) {
    let mut buf = [0u8; 4096];
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let n = stream.peek(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let upgrade = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("upgrade:") && l.to_ascii_lowercase().contains("websocket"));
    if !upgrade {
        let mut stream = stream;
        // consume the request before answering
        let mut sink = vec![0u8; n];
        let _ = stream.read(&mut sink);
        return serve_http(stream, &head, opts.web.as_deref(), ws_url);
    }
    let extra = opts.allow_origins.clone();
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
        if req.uri().path() != "/bridge" || !origin_ok(origin, &extra) {
            let mut e = ErrorResponse::new(Some("origin not allowed".into()));
            *e.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            return Err(e);
        }
        Ok(resp)
    };
    match tungstenite::accept_hdr(stream, check) {
        Ok(ws) => {
            let _ = ws.get_ref().set_read_timeout(Some(TIMEOUT));
            let mut l = link.lock().expect("bridge lock");
            if let Some(mut old) = l.ws.take() {
                let _ = old.close(None);
                let _ = old.flush();
            }
            l.ws = Some(ws);
            l.fresh = true;
            l.peer = peer.clone();
            l.connections += 1;
            eprintln!("shotstage: editor connected from {peer}");
        }
        Err(e) => eprintln!("shotstage: bridge handshake refused: {e}"),
    }
}

impl Bridge {
    /// Start listening. Fails if the port is taken.
    pub fn start(local: Local, opts: Options) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", opts.port))
            .map_err(|e| format!("cannot listen on 127.0.0.1:{}: {e}", opts.port))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let ws_url = format!("ws://127.0.0.1:{port}/bridge");
        let link = Arc::new(Mutex::new(Link::default()));
        let web = opts.web.is_some();
        let opts = Arc::new(opts);
        {
            let (link, ws_url) = (link.clone(), ws_url.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (link, opts, ws_url) = (link.clone(), opts.clone(), ws_url.clone());
                    std::thread::spawn(move || handle(stream, &link, &opts, &ws_url));
                }
            });
        }
        Ok(Self { local, link, url: format!("http://127.0.0.1:{port}/"), web })
    }

    pub fn ws_url(&self) -> String {
        format!("{}bridge", self.url.replace("http://", "ws://"))
    }

    /// `{connected, peer, url, ws}` for people and agents.
    pub fn status(&self) -> Value {
        let l = self.link.lock().expect("bridge lock");
        json!({
            "connected": l.ws.is_some(),
            "peer": if l.ws.is_some() { Some(l.peer.clone()) } else { None },
            "connections": l.connections,
            "editorUrl": if self.web { Some(self.url.clone()) } else { None },
            "ws": self.ws_url(),
            "howToConnect": if self.web {
                format!("open {} in a browser", self.url)
            } else {
                format!("in the editor page: connectBridge(editor, \"{}\") from bridge.js, or open it with ?bridge={}", self.ws_url(), self.ws_url())
            },
        })
    }

    fn send(link: &mut Link, req: &Value) -> CmdResult<Outcome> {
        let ws = link.ws.as_mut().ok_or_else(|| CommandError::new("unsupported", "no editor connected"))?;
        link.next_id += 1;
        let id = link.next_id;
        let msg = json!({ "id": id, "request": req }).to_string();
        let lost = |e: String| {
            CommandError::new(
                "failed",
                format!(
                    "lost the browser editor ({e}); the command may not have run there: check with scene_summary and retry"
                ),
            )
        };
        ws.send(Message::text(msg)).map_err(|e| lost(e.to_string()))?;
        loop {
            match ws.read().map_err(|e| lost(e.to_string()))? {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(&t).map_err(|e| lost(format!("bad reply: {e}")))?;
                    if v.get("id").and_then(Value::as_u64) != Some(id) {
                        continue; // a late reply to something that timed out
                    }
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        let o = &v["outcome"];
                        return Ok(Outcome {
                            result: o.get("result").cloned().unwrap_or(Value::Null),
                            revision: o.get("revision").and_then(Value::as_u64).unwrap_or(0),
                            changed: o.get("changed").and_then(Value::as_bool).unwrap_or(false),
                        });
                    }
                    let e = &v["error"];
                    return Err(CommandError::new(
                        e.get("code").and_then(Value::as_str).unwrap_or("failed"),
                        e.get("message").and_then(Value::as_str).unwrap_or("the editor reported an error"),
                    ));
                }
                Message::Close(_) => return Err(lost("closed".into())),
                _ => {}
            }
        }
    }

    /// Whether the editor is still there: reads whatever it sent since the last request without
    /// waiting, so a tab closed in between is noticed before a command is sent to it.
    fn alive(link: &mut Link) -> bool {
        let Some(ws) = link.ws.as_mut() else { return false };
        if ws.get_ref().set_nonblocking(true).is_err() {
            return false;
        }
        let ok = loop {
            match ws.read() {
                Ok(Message::Close(_)) => break false,
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break true,
                Err(_) => break false,
            }
        };
        if ok && ws.get_ref().set_nonblocking(false).is_ok() {
            return true;
        }
        link.ws = None;
        eprintln!("shotstage: editor disconnected");
        false
    }

    /// Run on the browser when one is connected (syncing first if it just connected).
    /// `Ok(None)`: no browser, run locally.
    fn forward(&mut self, req: &Value) -> CmdResult<Option<Outcome>> {
        let link = self.link.clone();
        let mut l = link.lock().expect("bridge lock");
        if !Self::alive(&mut l) {
            return Ok(None);
        }
        if l.fresh {
            l.fresh = false;
            let synced = Self::send(&mut l, &json!({"command": "scene.inspect"})).and_then(|o| {
                let browser: Scene =
                    serde_json::from_value(o.result).map_err(|e| CommandError::new("failed", e.to_string()))?;
                if browser.entities.is_empty() && !self.local.session.scene().entities.is_empty() {
                    // an empty editor opens what the agent has been working on
                    let scene = serde_json::to_value(self.local.session.scene()).unwrap_or(Value::Null);
                    Self::send(&mut l, &json!({"command": "scene.load", "params": {"scene": scene}}))?;
                } else {
                    // otherwise the editor the person has open wins
                    self.local.session = Session::new(browser);
                }
                Ok(())
            });
            if let Err(e) = synced {
                l.ws = None;
                return Err(e);
            }
        }
        match Self::send(&mut l, req) {
            Ok(o) => Ok(Some(o)),
            Err(e) => {
                if e.message.starts_with("lost the browser") {
                    l.ws = None;
                    eprintln!("shotstage: editor disconnected");
                }
                Err(e)
            }
        }
    }
}

impl Backend for Bridge {
    fn request(&mut self, req: Value) -> CmdResult<Outcome> {
        match self.forward(&req)? {
            Some(o) => Ok(o),
            None => self.local.request(req),
        }
    }
    fn scene(&mut self) -> CmdResult<Scene> {
        if let Some(o) = self.forward(&json!({"command": "scene.inspect"}))? {
            let s: Scene = serde_json::from_value(o.result).map_err(|e| CommandError::new("failed", e.to_string()))?;
            // keep the local copy current, so work continues if the browser goes away
            self.local.session = Session::new(s.clone());
            return Ok(s);
        }
        self.local.scene()
    }
    fn load(&mut self, scene: Scene) -> CmdResult<Outcome> {
        let v = serde_json::to_value(&scene).map_err(|e| CommandError::new("failed", e.to_string()))?;
        match self.forward(&json!({"command": "scene.load", "params": {"scene": v}}))? {
            Some(o) => {
                self.local.session = Session::new(scene);
                Ok(o)
            }
            None => self.local.load(scene),
        }
    }
    fn bridge_status(&self) -> Option<Value> {
        Some(self.status())
    }
    fn kind(&self) -> &'static str {
        if self.link.lock().map(|l| l.ws.is_some()).unwrap_or(false) { "browser" } else { "local" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;

    #[test]
    fn origins() {
        assert!(origin_ok(None, &[]));
        assert!(origin_ok(Some("http://localhost:8080"), &[]));
        assert!(origin_ok(Some("http://127.0.0.1:7777"), &[]));
        assert!(!origin_ok(Some("https://evil.example"), &[]));
        assert!(!origin_ok(Some("http://localhost.evil.example"), &[]));
        assert!(origin_ok(Some("https://app.example"), &["https://app.example".into()]));
    }

    /// A fake browser editor: a session behind a real WebSocket.
    fn fake_editor(ws_url: &str) -> std::thread::JoinHandle<Session> {
        let url = ws_url.to_string();
        std::thread::spawn(move || {
            let (mut ws, _) = tungstenite::connect(url).expect("connect");
            let mut s = Session::default();
            s.dispatch("entity.create", json!({"type": "mesh", "id": "browser_box"})).unwrap();
            loop {
                let msg = match ws.read() {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };
                let v: Value = serde_json::from_str(&msg).unwrap();
                if v["request"]["command"] == "bye" {
                    break;
                }
                let reply = match s.dispatch_json(v["request"].clone()) {
                    Ok(o) => {
                        json!({"id": v["id"], "ok": true, "outcome": {"result": o.result, "revision": o.revision, "changed": o.changed}})
                    }
                    Err(e) => json!({"id": v["id"], "ok": false, "error": {"code": e.code, "message": e.message}}),
                };
                ws.send(Message::text(reply.to_string())).unwrap();
            }
            s
        })
    }

    #[test]
    fn commands_run_in_the_connected_editor() {
        let mut local = Local::default();
        local.session.dispatch("entity.create", json!({"type": "camera", "id": "local_cam"})).unwrap();
        let bridge = Bridge::start(local, Options { port: 0, web: None, allow_origins: vec![] }).unwrap();
        let ws = bridge.ws_url();
        // the bridge page answers plain HTTP too
        let mut http = TcpStream::connect(bridge.url.trim_start_matches("http://").trim_end_matches('/')).unwrap();
        http.write_all(b"GET /bridge.json HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut body = String::new();
        let _ = http.read_to_string(&mut body);
        assert!(body.contains(&ws), "{body}");

        let mut host = Host::new(Box::new(bridge), None, false);
        // no editor yet: runs locally
        assert_eq!(host.backend.kind(), "local");
        host.call("entity.create", json!({"type": "empty", "id": "local_only"})).unwrap();

        let editor = fake_editor(&ws);
        for _ in 0..100 {
            if host.backend.kind() == "browser" {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(host.backend.kind(), "browser");
        // the browser had a scene, so it wins; the command runs there
        host.call("entity.create", json!({"type": "camera", "id": "agent_cam"})).unwrap();
        let s = host.backend.scene().unwrap();
        assert!(s.contains("browser_box") && s.contains("agent_cam") && !s.contains("local_cam"));
        let e = host.call("entity.delete", json!({"id": "ghost"})).unwrap_err();
        assert_eq!(e.code, "failed");
        host.call("bye", json!({})).unwrap_err();
        let browser = editor.join().unwrap();
        assert!(browser.scene().contains("agent_cam"));
        // the browser went away: back to the local copy, which kept up
        let after = host.call("entity.create", json!({"type": "empty", "id": "after2"}));
        assert!(after.is_ok());
        assert_eq!(host.backend.kind(), "local");
        let s = host.backend.scene().unwrap();
        assert!(s.contains("agent_cam") && s.contains("after2"));
    }
}
