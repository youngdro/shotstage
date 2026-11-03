//! Minimal MCP server over stdio (JSON-RPC 2.0, one message per line).
//!
//! Every registry command is published as its own tool (`entity.create` → `entity_create`,
//! with the command's JSON Schema as `inputSchema`), plus generic `command_list`,
//! `command_run` and `command_batch` tools and a `scene_save` tool.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use shotstage_commands::{Call, CommandInfo, Session};

const PROTOCOL_VERSION: &str = "2025-06-18";

fn tool_name(command: &str) -> String {
    command.replace('.', "_")
}

fn input_schema(params: &Value) -> Value {
    let mut s = params.clone();
    if let Some(o) = s.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.entry("type").or_insert(json!("object"));
    }
    s
}

fn tools(list: &[CommandInfo], can_save: bool) -> Vec<Value> {
    let mut t = vec![
        json!({
            "name": "command_list",
            "description": "List every shotstage command with its parameter JSON Schema.",
            "inputSchema": {"type": "object", "properties": {"filter": {"type": "string", "description": "Only ids starting with this prefix, e.g. `camera.`"}}},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "command_run",
            "description": "Run any shotstage command by id with JSON params.",
            "inputSchema": {"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string"}, "params": {"type": "object"}}},
        }),
        json!({
            "name": "command_batch",
            "description": "Run several commands as one transaction (all or nothing, one undo step).",
            "inputSchema": {"type": "object", "required": ["steps"], "properties": {
                "steps": {"type": "array", "items": {"type": "object", "required": ["command"], "properties": {
                    "command": {"type": "string"}, "params": {"type": "object"}}}}}},
        }),
    ];
    if can_save {
        t.push(json!({
            "name": "scene_save",
            "description": "Write the scene to the file the server was started with.",
            "inputSchema": {"type": "object", "properties": {}},
        }));
    }
    for c in list {
        let mut tool = json!({
            "name": tool_name(&c.id),
            "description": c.summary,
            "inputSchema": input_schema(&c.params),
        });
        if !c.mutates {
            tool["annotations"] = json!({"readOnlyHint": true});
        }
        t.push(tool);
    }
    t
}

fn text_result(v: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default()}],
        "structuredContent": if v.is_object() { v.clone() } else { json!({"result": v}) },
        "isError": is_error,
    })
}

fn call_tool(session: &mut Session, save_to: &Option<PathBuf>, name: &str, args: Value) -> Value {
    let list = session.list();
    let outcome = match name {
        "command_list" => {
            let f = args.get("filter").and_then(Value::as_str).unwrap_or("");
            let items: Vec<_> = list.into_iter().filter(|c| c.id.starts_with(f)).collect();
            return text_result(&json!({"commands": items}), false);
        }
        "scene_save" => {
            return match save_to {
                Some(p) => match crate::save_scene(p, session.scene()) {
                    Ok(()) => text_result(&json!({"saved": p.display().to_string()}), false),
                    Err(e) => text_result(&json!({"error": e}), true),
                },
                None => text_result(&json!({"error": "server was started without --scene"}), true),
            };
        }
        "command_run" => {
            let Some(cmd) = args.get("command").and_then(Value::as_str) else {
                return text_result(&json!({"error": "missing `command`"}), true);
            };
            session.dispatch(cmd, args.get("params").cloned().unwrap_or(json!({})))
        }
        "command_batch" => match serde_json::from_value::<Vec<Call>>(args.get("steps").cloned().unwrap_or(Value::Null))
        {
            Ok(steps) => session.batch(steps),
            Err(e) => return text_result(&json!({"error": format!("bad steps: {e}")}), true),
        },
        other => match list.iter().find(|c| tool_name(&c.id) == other) {
            Some(c) => session.dispatch(&c.id.clone(), args),
            None => return text_result(&json!({"error": format!("unknown tool `{other}`")}), true),
        },
    };
    match outcome {
        Ok(o) => {
            if o.changed
                && let Some(p) = save_to
                && let Err(e) = crate::save_scene(p, session.scene())
            {
                return text_result(&json!({"result": o.result, "saveError": e}), true);
            }
            text_result(&json!({"result": o.result, "revision": o.revision, "changed": o.changed}), false)
        }
        Err(e) => text_result(&json!({"error": {"code": e.code, "message": e.message}}), true),
    }
}

/// Handle one JSON-RPC message; `None` for notifications.
pub fn handle(session: &mut Session, save_to: &Option<PathBuf>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "shotstage", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "shotstage is a 3D previs editor. Build a scene (entity_create, entity_setTransform, mesh_addModifier), place cameras (camera_lookAt, camera_describe), animate (keyframe_set, camera_dollyMove, camera_orbitMove) and read results (scene_summary, camera_trajectory). Every edit is undoable with history_undo. Units: metres, degrees, frames.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools(&session.list(), save_to.is_some())})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(call_tool(session, save_to, &name, args))
        }
        m if m.starts_with("notifications/") => return None,
        m => Err(json!({"code": -32601, "message": format!("method not found: {m}")})),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

pub fn serve(mut session: Session, save_to: Option<PathBuf>) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut session, &save_to, &msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            ),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_round_trip() {
        let mut s = Session::default();
        let init =
            handle(&mut s, &None, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "shotstage");
        assert!(handle(&mut s, &None, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = handle(&mut s, &None, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "entity_create"));
        for t in tools {
            let n = t["name"].as_str().unwrap();
            assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{n}");
            assert_eq!(t["inputSchema"]["type"], "object", "{n}");
        }
        let call = handle(
            &mut s,
            &None,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "entity_create", "arguments": {"type": "camera", "position": [0, 1.6, 4]}}}),
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert_eq!(s.scene().entities.len(), 1);
        let bad = handle(
            &mut s,
            &None,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "entity_delete", "arguments": {"id": "ghost"}}}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
    }
}
