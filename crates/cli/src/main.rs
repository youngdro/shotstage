//! `shotstage` command line.
//!
//! ```text
//! shotstage commands                         # every command with its JSON Schema
//! shotstage run entity.create '{"type":"mesh"}' --scene shot.json
//! shotstage batch steps.json --scene shot.json
//! shotstage serve --scene shot.json          # JSON lines on stdin/stdout
//! shotstage mcp --scene shot.json            # MCP server on stdio
//! shotstage snapshot --scene shot.json -o frame.png --pass depth
//! ```

mod mcp;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use shotstage_commands::snapshot::{SNAPSHOT_COMMAND, SnapshotPass, plan, result_json};
use shotstage_commands::{Call, CommandError, Outcome, Session};
use shotstage_core::Scene;
use shotstage_render::{Headless, Pass, View};

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
    /// Render the view through a camera to a PNG file (needs a GPU or a software Vulkan driver).
    Snapshot {
        #[command(flatten)]
        scene: SceneArgs,
        /// Output PNG path.
        #[arg(long, short)]
        out: PathBuf,
        /// Camera id. Default: the camera in use at the frame.
        #[arg(long)]
        camera: Option<String>,
        #[arg(long)]
        frame: Option<f32>,
        /// clay, depth or normal.
        #[arg(long, default_value = "clay")]
        pass: String,
        #[arg(long)]
        width: Option<u32>,
        #[arg(long)]
        height: Option<u32>,
    },
}

/// Lazily created GPU for `render.snapshot`.
#[derive(Default)]
pub(crate) struct Gpu(Option<Headless>);

impl Gpu {
    fn snapshot_png(&mut self, scene: &Scene, params: Value) -> Result<(Value, Vec<u8>), CommandError> {
        let plan = plan(scene, params, None)?;
        if self.0.is_none() {
            self.0 = Some(Headless::new().map_err(|e| CommandError::new("unsupported", e))?);
        }
        let gpu = self.0.as_mut().expect("just set");
        let pass = match plan.pass {
            SnapshotPass::Clay => Pass::Clay,
            SnapshotPass::Depth => Pass::Depth,
            SnapshotPass::Normal => Pass::Normal,
        };
        let view = View { view: plan.view, proj: plan.proj, depth_near: 0.0, depth_far: 0.0 };
        let png = gpu
            .png(scene, plan.frame, view, pass, &plan.camera, (plan.width, plan.height))
            .map_err(|e| CommandError::new("failed", e))?;
        Ok((result_json(&plan, shotstage_render::base64(&png)), png))
    }

    /// Like `Session::dispatch`, but renders `render.snapshot` here.
    pub(crate) fn dispatch(
        &mut self,
        session: &mut Session,
        command: &str,
        params: Value,
    ) -> Result<Outcome, CommandError> {
        if command == SNAPSHOT_COMMAND {
            let (result, _) = self.snapshot_png(session.scene(), params)?;
            return Ok(Outcome { result, revision: session.revision(), changed: false });
        }
        session.dispatch(command, params)
    }

    pub(crate) fn dispatch_json(&mut self, session: &mut Session, request: Value) -> Result<Outcome, CommandError> {
        if request.get("command").and_then(Value::as_str) == Some(SNAPSHOT_COMMAND) {
            return self.dispatch(session, SNAPSHOT_COMMAND, request.get("params").cloned().unwrap_or(json!({})));
        }
        session.dispatch_json(request)
    }
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
            finish(Gpu::default().dispatch(&mut session, &command, params), &mut session, &scene)
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
            let mut gpu = Gpu::default();
            for line in stdin.lock().lines() {
                let line = line.map_err(|e| e.to_string())?;
                if line.trim().is_empty() {
                    continue;
                }
                let reply = serve_line(&mut session, &mut gpu, &line, &scene);
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
        Cmd::Snapshot { scene, out, camera, frame, pass, width, height } => {
            let session = scene.open()?;
            let params = json!({ "camera": camera, "frame": frame, "pass": pass, "width": width, "height": height });
            let params = Value::Object(
                params.as_object().cloned().unwrap_or_default().into_iter().filter(|(_, v)| !v.is_null()).collect(),
            );
            match Gpu::default().snapshot_png(session.scene(), params) {
                Ok((mut meta, png)) => {
                    std::fs::write(&out, png).map_err(|e| format!("writing {}: {e}", out.display()))?;
                    meta["data"] = json!(out.display().to_string());
                    print_json(&json!({ "ok": true, "result": meta }));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    print_json(&error_json(&e));
                    Ok(ExitCode::from(1))
                }
            }
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
fn serve_line(session: &mut Session, gpu: &mut Gpu, line: &str, scene: &SceneArgs) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return error_json(&CommandError::invalid(format!("request is not JSON: {e}"))),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let mut body = req.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("id");
    }
    let mut reply = match gpu.dispatch_json(session, body) {
        Ok(o) => {
            let saved = if o.changed { scene.save(session).err() } else { None };
            json!({ "ok": true, "result": o.result, "revision": o.revision, "changed": o.changed, "saveError": saved })
        }
        Err(e) => error_json(&e),
    };
    reply["id"] = id;
    reply
}
