//! `shotstage` command line.
//!
//! ```text
//! shotstage commands                         # every command with its JSON Schema
//! shotstage run entity.create '{"type":"mesh"}' --scene shot.json
//! shotstage batch steps.json --scene shot.json
//! shotstage serve --scene shot.json          # JSON lines on stdin/stdout
//! shotstage mcp --scene shot.json            # MCP server on stdio
//! ```

mod mcp;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use shotstage_commands::{Call, CommandError, Session};
use shotstage_core::Scene;

#[derive(Parser)]
#[command(
    name = "shotstage",
    version,
    about = "3D previs for AI image and video: every editor operation from the command line"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print every command with its parameter JSON Schema.
    Commands {
        /// Only ids and summaries.
        #[arg(long)]
        brief: bool,
    },
    /// Run one command and print its JSON result.
    Run {
        /// Command id, e.g. `entity.create`.
        command: String,
        /// Params as JSON (default `{}`); `-` reads stdin.
        params: Option<String>,
        #[command(flatten)]
        scene: SceneArgs,
    },
    /// Run a JSON array of `{"command", "params"}` steps as one transaction.
    Batch {
        /// File with the steps; `-` reads stdin.
        file: String,
        #[command(flatten)]
        scene: SceneArgs,
    },
    /// Long-lived session speaking JSON lines on stdin/stdout.
    Serve {
        #[command(flatten)]
        scene: SceneArgs,
    },
    /// MCP server on stdio (for Claude, Cursor and other MCP clients).
    Mcp {
        #[command(flatten)]
        scene: SceneArgs,
    },
}

#[derive(clap::Args)]
struct SceneArgs {
    /// Scene file to open. Created if missing.
    #[arg(long, short)]
    scene: Option<PathBuf>,
    /// Do not write changes back to the scene file.
    #[arg(long)]
    dry_run: bool,
}

impl SceneArgs {
    fn open(&self) -> Result<Session, String> {
        let Some(path) = &self.scene else { return Ok(Session::default()) };
        if !path.exists() {
            return Ok(Session::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let scene: Scene = serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        Ok(Session::new(scene))
    }

    fn save(&self, session: &Session) -> Result<(), String> {
        match &self.scene {
            Some(path) if !self.dry_run => save_scene(path, session.scene()),
            _ => Ok(()),
        }
    }
}

pub(crate) fn save_scene(path: &Path, scene: &Scene) -> Result<(), String> {
    let text = serde_json::to_string_pretty(scene).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn read_arg(s: &str) -> Result<String, String> {
    if s == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    } else {
        Ok(s.to_string())
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn error_json(e: &CommandError) -> Value {
    json!({ "ok": false, "error": { "code": e.code, "message": e.message } })
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("shotstage: {msg}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Commands { brief } => {
            let list = Session::default().list();
            if brief {
                for c in list {
                    println!("{:<26} {}{}", c.id, c.summary, if c.mutates { "" } else { "  (query)" });
                }
            } else {
                print_json(&json!(list));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { command, params, scene } => {
            let mut session = scene.open()?;
            let params = match params {
                Some(p) => serde_json::from_str(&read_arg(&p)?).map_err(|e| format!("params are not JSON: {e}"))?,
                None => json!({}),
            };
            finish(session.dispatch(&command, params), &mut session, &scene)
        }
        Cmd::Batch { file, scene } => {
            let mut session = scene.open()?;
            let text =
                if file == "-" { read_arg("-")? } else { std::fs::read_to_string(&file).map_err(|e| e.to_string())? };
            let steps: Vec<Call> = serde_json::from_str(&text).map_err(|e| format!("batch file: {e}"))?;
            finish(session.batch(steps), &mut session, &scene)
        }
        Cmd::Serve { scene } => {
            let mut session = scene.open()?;
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line.map_err(|e| e.to_string())?;
                if line.trim().is_empty() {
                    continue;
                }
                let reply = serve_line(&mut session, &line, &scene);
                writeln!(out, "{reply}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp { scene } => {
            let session = scene.open()?;
            mcp::serve(session, scene.scene.clone().filter(|_| !scene.dry_run))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn finish(
    r: Result<shotstage_commands::Outcome, CommandError>,
    session: &mut Session,
    scene: &SceneArgs,
) -> Result<ExitCode, String> {
    match r {
        Ok(o) => {
            if o.changed {
                scene.save(session)?;
            }
            print_json(&json!({ "ok": true, "result": o.result, "changed": o.changed }));
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            print_json(&error_json(&e));
            Ok(ExitCode::from(1))
        }
    }
}

/// One JSON-lines request: `{"id": 1, "command": "...", "params": {...}}` or
/// `{"id": 1, "batch": [...]}`. The reply echoes `id`.
fn serve_line(session: &mut Session, line: &str, scene: &SceneArgs) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return error_json(&CommandError::invalid(format!("request is not JSON: {e}"))),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let mut body = req.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("id");
    }
    let mut reply = match session.dispatch_json(body) {
        Ok(o) => {
            let saved = if o.changed { scene.save(session).err() } else { None };
            json!({ "ok": true, "result": o.result, "revision": o.revision, "changed": o.changed, "saveError": saved })
        }
        Err(e) => error_json(&e),
    };
    reply["id"] = id;
    reply
}
