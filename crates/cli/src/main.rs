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
//! shotstage render --scene shot.json -o frames/ --pass clay,depth --video   # every frame
//! shotstage mcp --scene shot.json --live --web dist   # drive the editor open in a browser
//! ```
//!
//! Every subcommand goes through [`host::Host`], so the CLI, the JSON-lines server and the MCP
//! server accept exactly the catalog that `shotstage commands` prints.

mod bridge;
mod host;
mod mcp;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use shotstage_commands::{Call, CommandError, Outcome, Session};

use crate::host::{Host, Local};

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
        #[command(flatten)]
        live: LiveArgs,
    },
    /// MCP server on stdio (for Claude, Cursor and other MCP clients).
    Mcp {
        #[command(flatten)]
        scene: SceneArgs,
        #[command(flatten)]
        live: LiveArgs,
        /// Publish only the generic tools (command_list / command_run / command_batch and a few
        /// essentials) instead of one tool per command, for clients with small tool budgets.
        #[arg(long)]
        compact: bool,
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
    /// Render every frame through the shot cameras (following the shots, else the active camera)
    /// as numbered PNGs per pass, plus camera.json; with --video also an MP4 per pass (ffmpeg).
    Render {
        #[command(flatten)]
        scene: SceneArgs,
        /// Output folder (created if missing).
        #[arg(long, short)]
        out: PathBuf,
        /// Passes: clay, depth, normal (comma separated).
        #[arg(long, value_delimiter = ',', default_value = "clay")]
        pass: Vec<String>,
        #[arg(long, default_value_t = 1280)]
        width: u32,
        /// First frame. Default: the timeline start.
        #[arg(long)]
        start: Option<u32>,
        /// Last frame. Default: the timeline end.
        #[arg(long)]
        end: Option<u32>,
        /// Also encode clay.mp4 / depth.mp4 / normal.mp4 with ffmpeg (if it is installed).
        #[arg(long)]
        video: bool,
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
        /// Also render the normal pass of every frame.
        #[arg(long)]
        normal: bool,
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

#[derive(clap::Args)]
struct LiveArgs {
    /// Drive an editor open in a browser: listen on 127.0.0.1 for it to connect (see --port,
    /// --web). Until one connects, commands run on the local scene.
    #[arg(long)]
    live: bool,
    /// Bridge port (0 picks a free one).
    #[arg(long, default_value_t = 7777)]
    port: u16,
    /// Serve the built web editor (the dist/ folder) at http://127.0.0.1:<port>/; the page
    /// connects to the bridge by itself.
    #[arg(long)]
    web: Option<PathBuf>,
    /// Another page origin allowed to connect (localhost pages always are), e.g. https://my.app
    #[arg(long)]
    allow_origin: Vec<String>,
}

/// Drop null fields so optional params stay unset.
fn compact(v: Value) -> Value {
    Value::Object(v.as_object().cloned().unwrap_or_default().into_iter().filter(|(_, v)| !v.is_null()).collect())
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
    fn session(&self) -> Result<Session, String> {
        let Some(path) = &self.scene else { return Ok(Session::default()) };
        if !path.exists() {
            return Ok(Session::default());
        }
        host::read_scene(path).map(Session::new).map_err(|e| e.message)
    }

    /// A host over the local scene that writes changes back to the scene file.
    fn host(&self) -> Result<Host, String> {
        Ok(Host::new(Box::new(Local::new(self.session()?)), self.scene.clone(), !self.dry_run))
    }

    /// Like [`SceneArgs::host`], with the live bridge when asked for.
    fn live_host(&self, live: &LiveArgs) -> Result<(Host, Option<Value>), String> {
        if !live.live {
            return Ok((self.host()?, None));
        }
        let web = live.web.clone().or_else(|| std::env::var_os("SHOTSTAGE_WEB").map(PathBuf::from));
        if let Some(w) = &web
            && !w.join("index.html").exists()
        {
            return Err(format!("{} has no index.html: build it with scripts/build-web.sh", w.display()));
        }
        let b = bridge::Bridge::start(
            Local::new(self.session()?),
            bridge::Options { port: live.port, web, allow_origins: live.allow_origin.clone() },
        )?;
        let status = b.status();
        eprintln!("shotstage: live bridge on {} ({})", b.url, status["howToConnect"].as_str().unwrap_or_default());
        Ok((Host::new(Box::new(b), self.scene.clone(), !self.dry_run), Some(status)))
    }
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

/// Print a result the way every subcommand does: `{ok, result, changed}` or `{ok: false, error}`.
fn finish(r: Result<Outcome, CommandError>) -> ExitCode {
    match r {
        Ok(o) => {
            print_json(&json!({ "ok": true, "result": o.result, "changed": o.changed }));
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_json(&error_json(&e));
            ExitCode::from(1)
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
                    let tag = if !c.needs.is_empty() {
                        format!("  ({})", c.needs.join(", "))
                    } else if !c.mutates {
                        "  (query)".into()
                    } else {
                        String::new()
                    };
                    println!("{:<26} {}{tag}", c.id, c.summary);
                }
            } else {
                print_json(&json!(list));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { command, params, scene } => {
            let params = match params {
                Some(p) => serde_json::from_str(&read_arg(&p)?).map_err(|e| format!("params are not JSON: {e}"))?,
                None => json!({}),
            };
            Ok(finish(scene.host()?.call(&command, params)))
        }
        Cmd::Batch { file, scene } => {
            let text =
                if file == "-" { read_arg("-")? } else { std::fs::read_to_string(&file).map_err(|e| e.to_string())? };
            let steps: Vec<Call> = serde_json::from_str(&text).map_err(|e| format!("batch file: {e}"))?;
            Ok(finish(scene.host()?.request(json!({ "batch": steps }))))
        }
        Cmd::Serve { scene, live } => {
            let (mut host, _) = scene.live_host(&live)?;
            let stdin = std::io::stdin();
            let mut out = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let line = line.map_err(|e| e.to_string())?;
                if line.trim().is_empty() {
                    continue;
                }
                let reply = serve_line(&mut host, &line);
                writeln!(out, "{reply}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp { scene, live, compact } => {
            let (host, bridge) = scene.live_host(&live)?;
            mcp::serve(host, mcp::Config { compact, live: bridge.is_some() })?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Import { file, scene, id, position, scale, fit_height, unit, up_axis } => {
            let params = compact(json!({ "path": file, "id": id, "position": position,
                "scale": scale, "fitHeight": fit_height, "unit": unit, "upAxis": up_axis }));
            Ok(finish(scene.host()?.call("model.import", params)))
        }
        Cmd::Export { file, scene, ids, frame } => {
            let params = compact(json!({ "path": file, "ids": ids, "frame": frame }));
            Ok(finish(scene.host()?.call("model.export", params)))
        }
        Cmd::Render { scene, out, pass, width, start, end, video } => {
            let params = compact(
                json!({ "out": out, "passes": pass, "width": width, "start": start, "end": end, "video": video }),
            );
            Ok(finish(scene.host()?.call("render.sequence", params)))
        }
        Cmd::Storyboard { scene, out, width, depth, normal } => {
            let mut passes = vec!["clay"];
            if depth {
                passes.push("depth");
            }
            if normal {
                passes.push("normal");
            }
            let params = json!({ "out": out, "width": width, "passes": passes });
            let r = scene.host()?.call("render.storyboard", params).map(|mut o| {
                // the full document is in storyboard.json; print where it went
                let r = &o.result;
                o.result =
                    json!({ "folder": r["folder"], "index": r["index"], "shots": r["shots"].as_array().map(Vec::len) });
                o
            });
            Ok(finish(r))
        }
        Cmd::Snapshot { scene, out, camera, frame, pass, width, height } => {
            let params =
                compact(json!({ "camera": camera, "frame": frame, "pass": pass, "width": width, "height": height }));
            let mut host = scene.host()?;
            match host.snapshot(params) {
                Ok((mut meta, png)) => {
                    std::fs::write(&out, png).map_err(|e| format!("writing {}: {e}", out.display()))?;
                    meta["file"] = json!(out.display().to_string());
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

/// One JSON-lines request: `{"id": 1, "command": "...", "params": {...}}` or
/// `{"id": 1, "batch": [...]}`. The reply echoes `id`.
fn serve_line(host: &mut Host, line: &str) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return error_json(&CommandError::invalid(format!("request is not JSON: {e}"))),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let mut body = req.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("id");
    }
    let mut reply = match host.request(body) {
        Ok(o) => json!({ "ok": true, "result": o.result, "revision": o.revision, "changed": o.changed }),
        Err(e) => error_json(&e),
    };
    reply["id"] = id;
    reply
}
