//! `shotstage` command line.
//!
//! ```text
//! shotstage commands                         # every command with its JSON Schema
//! shotstage run entity.create '{"type":"mesh"}' --scene shot.json
//! shotstage batch steps.json --scene shot.json
//! shotstage serve --scene shot.json          # JSON lines on stdin/stdout
//! shotstage mcp --scene shot.json            # MCP server on stdio
//! shotstage snapshot --scene shot.json -o frame.png --pass depth
//! shotstage import chair.glb --scene shot.json --fit-height 0.9
//! shotstage export set.fbx --scene shot.json     # glb, gltf, obj, stl; fbx through Blender
//! shotstage storyboard --scene shot.json -o board/  # first / middle / last frame of every shot
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
    /// Import a model file (glb, gltf, obj, stl; fbx and blend through a local Blender).
    Import {
        file: PathBuf,
        #[command(flatten)]
        scene: SceneArgs,
        /// Group id. Default: from the file name.
        #[arg(long)]
        id: Option<String>,
        /// Bottom centre position `x,y,z` in metres.
        #[arg(long, value_delimiter = ',', num_args = 3)]
        position: Option<Vec<f32>>,
        /// Uniform scale.
        #[arg(long)]
        scale: Option<f32>,
        /// Scale to this height in metres.
        #[arg(long)]
        fit_height: Option<f32>,
        /// File unit for obj / stl: m, cm, mm, in or auto.
        #[arg(long)]
        unit: Option<String>,
        /// Up axis of the file: y or z.
        #[arg(long)]
        up_axis: Option<String>,
    },
    /// Render the first, middle and last frame of every shot (clay and depth) into a folder,
    /// with storyboard.json (shot descriptions and camera data) and index.html to look at.
    Storyboard {
        #[command(flatten)]
        scene: SceneArgs,
        /// Output folder (created if missing).
        #[arg(long, short)]
        out: PathBuf,
        /// Image width in pixels.
        #[arg(long, default_value_t = 960)]
        width: u32,
        /// Also render the depth pass of every frame.
        #[arg(long)]
        depth: bool,
    },
    /// Export the scene (or some entities) to a model file; the format comes from the extension.
    Export {
        file: PathBuf,
        #[command(flatten)]
        scene: SceneArgs,
        /// Only these entity ids (comma separated).
        #[arg(long, value_delimiter = ',')]
        ids: Option<Vec<String>>,
        #[arg(long)]
        frame: Option<f32>,
    },
}

/// A shot name usable in a file name.
fn compact_name(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(32).collect();
    if s.is_empty() { "shot".into() } else { s }
}

fn format_of(path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    shotstage_commands::model::ModelFormat::from_extension(&ext)
        .map(|f| f.extension().to_string())
        .ok_or_else(|| format!("unknown model format `.{ext}`: use glb, gltf, obj, stl, fbx or blend"))
}

/// Drop null fields so optional params stay unset.
fn compact(v: Value) -> Value {
    Value::Object(v.as_object().cloned().unwrap_or_default().into_iter().filter(|(_, v)| !v.is_null()).collect())
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
        Cmd::Import { file, scene, id, position, scale, fit_height, unit, up_axis } => {
            let mut session = scene.open()?;
            let format = format_of(&file)?;
            let bytes = std::fs::read(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
            let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
            let mut params = compact(json!({ "format": format, "name": name, "id": id, "position": position,
                "scale": scale, "fitHeight": fit_height, "unit": unit, "upAxis": up_axis }));
            params["data"] = json!(shotstage_commands::model::encode(&bytes));
            if format == "obj" {
                // pick up colours from the .mtl next to it
                let mtl = file.with_extension("mtl");
                if let Ok(text) = std::fs::read_to_string(&mtl) {
                    params["mtl"] = json!(text);
                }
            }
            finish(session.dispatch("model.import", params), &mut session, &scene)
        }
        Cmd::Export { file, scene, ids, frame } => {
            let mut session = scene.open()?;
            let format = format_of(&file)?;
            let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("scene").to_string();
            let params = compact(json!({ "format": format, "ids": ids, "frame": frame, "name": name }));
            match session.dispatch("model.export", params) {
                Ok(o) => {
                    let mut r = o.result;
                    let write = |path: &Path, bytes: &[u8]| {
                        std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
                    };
                    if let Some(d) = r.get("data").and_then(Value::as_str) {
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(d).map_err(|e| e.to_string())?;
                        write(&file, &bytes)?;
                    } else if let Some(t) = r.get("text").and_then(Value::as_str) {
                        write(&file, t.as_bytes())?;
                    }
                    if let Some(m) = r.get("mtl").and_then(Value::as_str) {
                        write(&file.with_extension("mtl"), m.as_bytes())?;
                    }
                    if let Some(o) = r.as_object_mut() {
                        o.remove("data");
                        o.remove("text");
                        o.remove("mtl");
                    }
                    r["file"] = json!(file.display().to_string());
                    print_json(&json!({ "ok": true, "result": r }));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    print_json(&error_json(&e));
                    Ok(ExitCode::from(1))
                }
            }
        }
        Cmd::Storyboard { scene, out, width, depth } => {
            let mut session = scene.open()?;
            std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
            let t = session.scene().timeline.clone();
            // every shot, or the whole timeline through the active camera
            let shots: Vec<(String, String, u32, u32)> = if t.shots.is_empty() {
                match session.scene().active_camera.clone() {
                    Some(c) => vec![("shot".into(), c, t.start, t.end)],
                    None => return Err("the scene has no shots and no active camera".into()),
                }
            } else {
                t.shots.iter().map(|s| (s.name.clone(), s.camera.clone(), s.start, s.end)).collect()
            };
            let mut gpu = Gpu::default();
            let mut board = vec![];
            let mut html = String::from(
                "<!doctype html><meta charset=utf-8><title>Storyboard</title><style>body{font:14px system-ui;background:#16171a;color:#ddd;margin:24px}h2{margin:28px 0 6px}.row{display:flex;gap:12px}figure{margin:0}img{width:100%;border-radius:6px;display:block}figcaption{color:#999;font-size:12px;margin-top:4px}</style><h1>Storyboard</h1>",
            );
            for (i, (name, cam, a, b)) in shots.iter().enumerate() {
                let mid = (a + b) / 2;
                let mut frames = vec![];
                html.push_str(&format!("<h2>{} · {name}</h2><div class=row>", i + 1));
                for (label, f) in [("first", *a), ("middle", mid), ("last", *b)] {
                    let mut files = vec![];
                    for pass in if depth { vec!["clay", "depth"] } else { vec!["clay"] } {
                        let file = format!("{:02}_{}_{label}_{pass}.png", i + 1, compact_name(name));
                        let (_, png) = gpu
                            .snapshot_png(
                                session.scene(),
                                json!({ "camera": cam, "frame": f, "pass": pass, "width": width }),
                            )
                            .map_err(|e| e.message)?;
                        std::fs::write(out.join(&file), png).map_err(|e| e.to_string())?;
                        files.push(file);
                    }
                    let d = session
                        .dispatch("camera.describe", json!({ "id": cam, "frame": f }))
                        .map(|o| o.result)
                        .unwrap_or(Value::Null);
                    let summary = d["summary"].as_str().unwrap_or_default().to_string();
                    html.push_str(&format!(
                        "<figure><img src=\"{}\"><figcaption>{label} · frame {f} · {summary}</figcaption></figure>",
                        files[0]
                    ));
                    frames.push(json!({ "position": label, "frame": f, "images": files, "describe": d }));
                }
                html.push_str("</div>");
                let traj = session
                    .dispatch("camera.trajectory", json!({ "id": cam, "start": a, "end": b }))
                    .map(|o| o.result["frames"].clone())
                    .unwrap_or(Value::Null);
                board.push(json!({ "shot": name, "camera": cam, "start": a, "end": b,
                    "seconds": (b - a + 1) as f32 / t.fps, "frames": frames, "trajectory": traj }));
            }
            let doc = json!({ "fps": t.fps, "shots": board });
            std::fs::write(out.join("storyboard.json"), serde_json::to_string_pretty(&doc).unwrap_or_default())
                .map_err(|e| e.to_string())?;
            std::fs::write(out.join("index.html"), html).map_err(|e| e.to_string())?;
            print_json(&json!({ "ok": true, "result": { "folder": out.display().to_string(), "shots": shots.len() } }));
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
