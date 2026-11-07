//! Minimal MCP server over stdio (JSON-RPC 2.0, one message per line).
//!
//! Every command of the catalog is published as its own tool (`entity.create` →
//! `entity_create`, with the command's JSON Schema as `inputSchema`), including the host
//! commands (render_snapshot, render_storyboard, render_sequence, scene_save, scene_open,
//! editor_*), plus generic `command_list`, `command_run` and `command_batch` tools. Pictures
//! come back as MCP image content, so the model can look at the shot it just set up. With
//! `--live`, commands run in the browser editor connected to the bridge (see `bridge.rs`).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use shotstage_commands::host::{STORYBOARD_COMMAND, StoryboardParams};
use shotstage_commands::snapshot::SNAPSHOT_COMMAND;
use shotstage_commands::{Call, CommandError, CommandInfo, Session, parse};

use crate::host::Host;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// How many storyboard pictures come back as images (the rest are files).
const BOARD_IMAGES: usize = 12;

#[derive(Clone, Copy, Default)]
pub struct Config {
    /// Only the generic tools and a few essentials.
    pub compact: bool,
    /// The bridge is on: publish `bridge_status`.
    pub live: bool,
}

/// Published one by one even with `--compact`.
const ESSENTIALS: &[&str] = &[
    "scene.summary",
    "scene.template",
    "prefab.create",
    "camera.frame",
    "motion.apply",
    "render.snapshot",
    "render.storyboard",
    "history.undo",
];

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

fn tools(list: &[CommandInfo], cfg: Config) -> Vec<Value> {
    let mut t = vec![
        json!({
            "name": "command_list",
            "description": "List every shotstage command with its parameter JSON Schema.",
            "inputSchema": {"type": "object", "properties": {"filter": {"type": "string", "description": "Only ids starting with this prefix, e.g. `camera.`"}}},
            "annotations": {"readOnlyHint": true},
        }),
        json!({
            "name": "command_run",
            "description": "Run any shotstage command by id with JSON params (see command_list).",
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
    if cfg.live {
        t.push(json!({
            "name": "bridge_status",
            "description": "Whether a browser editor is connected to the live bridge, and the address to open or connect it. While one is connected every command runs in it, so the person sees each change.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true},
        }));
    }
    for c in list.iter().filter(|c| !cfg.compact || ESSENTIALS.contains(&c.id.as_str())) {
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

fn error_result(e: &CommandError) -> Value {
    text_result(&json!({"error": {"code": e.code, "message": e.message}}), true)
}

/// A storyboard for the model: written to a folder, with the clay pictures as images.
fn storyboard(host: &mut Host, mut args: Value) -> Result<Value, CommandError> {
    // MCP always writes the board to a folder (default: next to the scene file)
    if parse::<StoryboardParams>(args.clone())?.out.is_none() {
        let d: PathBuf = match &host.scene_path {
            Some(s) => s.with_extension("storyboard"),
            None => std::env::temp_dir().join(format!("shotstage-storyboard-{}", std::process::id())),
        };
        args["out"] = json!(d.display().to_string());
    }
    let board = host.storyboard(args)?;
    let pictures: Vec<(String, Vec<u8>)> = board
        .plan
        .pictures
        .iter()
        .zip(&board.pngs)
        .filter(|(p, _)| p.pass == shotstage_commands::snapshot::SnapshotPass::Clay)
        .map(|(p, png)| (p.file.clone(), png.clone()))
        .collect();
    let mut doc = shotstage_commands::host::finish_storyboard(board.plan, &board.pngs)?;
    // the per-frame camera matrices are in storyboard.json; keep the reply readable
    if let Some(shots) = doc["shots"].as_array_mut() {
        for s in shots {
            if let Some(o) = s.as_object_mut() {
                o.remove("trajectory");
            }
        }
    }
    let mut content = vec![json!({"type": "text", "text": serde_json::to_string(&doc).unwrap_or_default()})];
    let step = pictures.len().div_ceil(BOARD_IMAGES).max(1);
    for (file, png) in pictures.iter().step_by(step) {
        content.push(json!({"type": "text", "text": file}));
        content.push(json!({"type": "image", "data": shotstage_render::base64(png), "mimeType": "image/png"}));
    }
    Ok(json!({ "content": content, "isError": false }))
}

fn call_tool(host: &mut Host, cfg: Config, name: &str, args: Value) -> Value {
    let list = Session::default().list();
    // unwrap command_run into the command it names
    let (name, args) = match name {
        "command_run" => match args.get("command").and_then(Value::as_str) {
            Some(cmd) => (tool_name(cmd), args.get("params").cloned().unwrap_or(json!({}))),
            None => return text_result(&json!({"error": "missing `command`"}), true),
        },
        _ => (name.to_string(), args),
    };
    match name.as_str() {
        "command_list" => {
            let f = args.get("filter").and_then(Value::as_str).unwrap_or("");
            let items: Vec<_> = list.into_iter().filter(|c| c.id.starts_with(f)).collect();
            return text_result(&json!({"commands": items}), false);
        }
        "bridge_status" if cfg.live => {
            return match host.bridge_status() {
                Some(s) => text_result(&s, false),
                None => text_result(&json!({"error": "the bridge is not running"}), true),
            };
        }
        "command_batch" => {
            return match serde_json::from_value::<Vec<Call>>(args.get("steps").cloned().unwrap_or(Value::Null)) {
                Ok(steps) => match host.request(json!({ "batch": steps })) {
                    Ok(o) => {
                        text_result(&json!({"result": o.result, "revision": o.revision, "changed": o.changed}), false)
                    }
                    Err(e) => error_result(&e),
                },
                Err(e) => text_result(&json!({"error": format!("bad steps: {e}")}), true),
            };
        }
        _ => {}
    }
    let Some(cmd) = list.iter().find(|c| tool_name(&c.id) == name).map(|c| c.id.clone()) else {
        return text_result(&json!({"error": format!("unknown tool `{name}`; call command_list")}), true);
    };
    match cmd.as_str() {
        SNAPSHOT_COMMAND => match host.snapshot(args) {
            Ok((meta, png)) => json!({
                "content": [
                    {"type": "image", "data": shotstage_render::base64(&png), "mimeType": "image/png"},
                    {"type": "text", "text": serde_json::to_string(&meta).unwrap_or_default()},
                ],
                "isError": false,
            }),
            Err(e) => error_result(&e),
        },
        STORYBOARD_COMMAND => storyboard(host, args).unwrap_or_else(|e| error_result(&e)),
        _ => match host.call(&cmd, args) {
            Ok(o) => {
                let mut r = json!({"result": o.result, "revision": o.revision, "changed": o.changed});
                if cfg.live {
                    r["ranIn"] = json!(host.backend.kind());
                }
                text_result(&r, false)
            }
            Err(e) => error_result(&e),
        },
    }
}

/// Handle one JSON-RPC message; `None` for notifications.
pub fn handle(host: &mut Host, cfg: Config, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "shotstage", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "shotstage is a 3D previs editor; every tool here is also a browser API call and a CLI command. Start from scene_template (see scene_templates) or build up. Prefer the high-level tools: prefab_create (real-size rigged man / woman, furniture, fresh-food retail fixtures and produce, product-shoot stand-ins, podiums and backdrops; see prefab_list), pose_set / pose_aim / person_setHair to pose people, model_import / model_export for glTF, OBJ, STL (FBX through Blender), light_preset or entity_create type light for lighting, image_import for reference pictures and backdrops, motion_apply for camera moves / object motion / character actions (see motion_list), entity_place (rest on / next to / drop), entity_face, camera_frame (shot size + angle + side), then look at the result with render_snapshot (returns an image) and camera_describe. shot_create / shot_cut lay out the edit; render_storyboard renders the first / middle / last frame of every shot (images back, files to a folder) and render_sequence renders every frame as clay / depth / normal control sequences with per-frame camera data. scene_save / scene_open / model_import / model_export / image_import take file paths. Low-level tools (entity_create, entity_setTransform, mesh_*, keyframe_set, camera_dollyMove, camera_orbitMove) give exact control. Read the scene with scene_summary and entity_bounds. Every edit is undoable with history_undo. Units: metres, degrees, frames; +Y up, object fronts face +Z.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools(&Session::default().list(), cfg)})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(call_tool(host, cfg, &name, args))
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

pub fn serve(mut host: Host, cfg: Config) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut host, cfg, &msg),
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
    use crate::host::Local;

    fn rpc(h: &mut Host, cfg: Config, id: u64, method: &str, params: Value) -> Value {
        handle(h, cfg, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap()
    }

    #[test]
    fn mcp_round_trip() {
        let mut h = Host::new(Box::new(Local::default()), None, false);
        let cfg = Config::default();
        let init = rpc(&mut h, cfg, 1, "initialize", json!({}));
        assert_eq!(init["result"]["serverInfo"]["name"], "shotstage");
        assert!(handle(&mut h, cfg, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = rpc(&mut h, cfg, 2, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().unwrap();
        // one tool per catalog entry, host commands included
        for c in Session::default().list() {
            assert!(tools.iter().any(|t| t["name"] == tool_name(&c.id)), "{}", c.id);
        }
        for t in tools {
            let n = t["name"].as_str().unwrap();
            assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{n}");
            assert!(n.len() <= 64, "{n}");
            assert_eq!(t["inputSchema"]["type"], "object", "{n}");
        }
        let call = rpc(
            &mut h,
            cfg,
            3,
            "tools/call",
            json!({"name": "entity_create", "arguments": {"type": "camera", "position": [0, 1.6, 4]}}),
        );
        assert_eq!(call["result"]["isError"], false);
        assert_eq!(h.backend.scene().unwrap().entities.len(), 1);
        let bad = rpc(&mut h, cfg, 4, "tools/call", json!({"name": "entity_delete", "arguments": {"id": "ghost"}}));
        assert_eq!(bad["result"]["isError"], true);
        // host commands work through the same tools
        let st = rpc(
            &mut h,
            cfg,
            5,
            "tools/call",
            json!({"name": "command_run",
            "arguments": {"command": "editor.setFrame", "params": {"frame": 7}}}),
        );
        assert_eq!(st["result"]["isError"], false);
        let st = rpc(&mut h, cfg, 6, "tools/call", json!({"name": "editor_state", "arguments": {}}));
        assert_eq!(st["result"]["structuredContent"]["result"]["frame"], 7.0);
        let save = rpc(&mut h, cfg, 7, "tools/call", json!({"name": "scene_save", "arguments": {}}));
        assert_eq!(save["result"]["isError"], true, "no scene file and no path");
    }

    #[test]
    fn compact_mode_keeps_the_essentials() {
        let mut h = Host::new(Box::new(Local::default()), None, false);
        let cfg = Config { compact: true, live: false };
        let list = rpc(&mut h, cfg, 1, "tools/list", json!({}));
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.len() < 20, "{names:?}");
        assert!(names.contains(&"command_run".to_string()) && names.contains(&"render_snapshot".to_string()));
        // everything else is still reachable through command_run
        let r = rpc(
            &mut h,
            cfg,
            2,
            "tools/call",
            json!({"name": "command_run",
            "arguments": {"command": "light.list"}}),
        );
        assert_eq!(r["result"]["isError"], false);
    }
}
