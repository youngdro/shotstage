//! The CLI's host: runs every command of the catalog, including the host commands (rendering,
//! files, the editor view), over a [`Backend`]: the local session, or a browser editor
//! connected through the live bridge. `run`, `batch`, `serve` and `mcp` all go through here, so
//! they accept exactly the same commands.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};
use shotstage_commands::host::{
    self, BoardPlan, OpenParams, SEQUENCE_COMMAND, STORYBOARD_COMMAND, SaveParams, ViewState,
};
use shotstage_commands::sheet::SHEET_COMMAND;
use shotstage_commands::snapshot::{SNAPSHOT_COMMAND, SnapshotPass, plan, result_json};
use shotstage_commands::{Call, CmdResult, CommandError, Outcome, Session, parse};
use shotstage_core::Scene;
use shotstage_render::{Headless, Pass, View};

/// Where commands actually run.
pub trait Backend {
    /// A request `{command, params, merge?}` or `{batch: [...]}`: document commands, history,
    /// `editor.*` and `render.snapshot` (whose result carries the PNG as base64 `data`).
    fn request(&mut self, req: Value) -> CmdResult<Outcome>;
    /// The current scene document.
    fn scene(&mut self) -> CmdResult<Scene>;
    /// Replace the scene (one undo step).
    fn load(&mut self, scene: Scene) -> CmdResult<Outcome>;
    /// `"local"` or `"browser"`.
    fn kind(&self) -> &'static str;
    /// The live bridge's status, if this backend is one.
    fn bridge_status(&self) -> Option<Value> {
        None
    }
}

/// Lazily created GPU for snapshots.
#[derive(Default)]
pub struct Gpu(Option<Headless>);

impl Gpu {
    pub fn snapshot_png(&mut self, scene: &Scene, params: Value, frame: Option<f32>) -> CmdResult<(Value, Vec<u8>)> {
        let plan = plan(scene, params, frame)?;
        if self.0.is_none() {
            self.0 = Some(Headless::new().map_err(|e| CommandError::new("unsupported", e))?);
        }
        let gpu = self.0.as_mut().expect("just set");
        let pass = match plan.pass {
            SnapshotPass::Clay => Pass::Clay,
            SnapshotPass::Depth => Pass::Depth,
            SnapshotPass::Normal => Pass::Normal,
        };
        let view = View { view: plan.view, proj: plan.proj, depth: plan.depth_range };
        let png = gpu
            .png(scene, plan.frame, view, pass, &plan.camera, (plan.width, plan.height))
            .map_err(|e| CommandError::new("failed", e))?;
        Ok((result_json(&plan, shotstage_render::base64(&png)), png))
    }
}

/// The session in this process, rendered on this machine's GPU.
#[derive(Default)]
pub struct Local {
    pub session: Session,
    pub gpu: Gpu,
    pub view: ViewState,
}

impl Local {
    pub fn new(session: Session) -> Self {
        Self { session, ..Default::default() }
    }
}

impl Backend for Local {
    fn request(&mut self, req: Value) -> CmdResult<Outcome> {
        let command = req.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let rev = self.session.revision();
        if command == SNAPSHOT_COMMAND {
            let (result, _) = self.gpu.snapshot_png(self.session.scene(), params, self.view.frame)?;
            return Ok(Outcome { result, revision: rev, changed: false });
        }
        if let Some(r) = host::view_command(&mut self.view, self.session.scene(), &command, params) {
            return r.map(|result| Outcome { result, revision: rev, changed: false });
        }
        let out = self.session.dispatch_json(req)?;
        // a selection that no longer exists is dropped, like the editor does
        if let Some(sel) = &self.view.selected
            && !self.session.scene().contains(sel)
        {
            self.view.selected = None;
        }
        Ok(out)
    }
    fn scene(&mut self) -> CmdResult<Scene> {
        Ok(self.session.scene().clone())
    }
    fn load(&mut self, scene: Scene) -> CmdResult<Outcome> {
        self.session.replace_scene(scene);
        Ok(Outcome { result: Value::Null, revision: self.session.revision(), changed: true })
    }
    fn kind(&self) -> &'static str {
        "local"
    }
}

pub struct Host {
    pub backend: Box<dyn Backend>,
    /// The scene file: default target of `scene.save`, and where changes are written back.
    pub scene_path: Option<PathBuf>,
    /// Write the scene file after every change.
    pub autosave: bool,
}

fn fail(e: impl std::fmt::Display) -> CommandError {
    CommandError::new("failed", e.to_string())
}

pub fn save_scene(path: &Path, scene: &Scene) -> CmdResult<()> {
    let text = serde_json::to_string_pretty(scene).map_err(fail)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| fail(format!("creating {}: {e}", dir.display())))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| fail(format!("writing {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| fail(format!("writing {}: {e}", path.display())))
}

pub fn read_scene(path: &Path) -> CmdResult<Scene> {
    let text = std::fs::read_to_string(path).map_err(|e| fail(format!("reading {}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| CommandError::invalid(format!("parsing {}: {e}", path.display())))
}

fn png_of(result: &Value) -> CmdResult<Vec<u8>> {
    let b64 = result.get("data").and_then(Value::as_str).ok_or_else(|| fail("the snapshot has no picture"))?;
    base64::engine::general_purpose::STANDARD.decode(b64).map_err(fail)
}

/// A finished storyboard: its document and, per rendered picture, the PNG.
pub struct Board {
    pub plan: BoardPlan,
    pub pngs: Vec<Vec<u8>>,
}

impl Host {
    pub fn new(backend: Box<dyn Backend>, scene_path: Option<PathBuf>, autosave: bool) -> Self {
        Self { backend, scene_path, autosave }
    }

    pub fn bridge_status(&self) -> Option<Value> {
        self.backend.bridge_status()
    }

    /// Run one command of the catalog.
    pub fn call(&mut self, command: &str, params: Value) -> CmdResult<Outcome> {
        self.request(json!({ "command": command, "params": params }))
    }

    /// Run a request: `{command, params, merge?}` or `{batch: [...]}`.
    pub fn request(&mut self, req: Value) -> CmdResult<Outcome> {
        let out = self.request_inner(req)?;
        if out.changed && self.autosave {
            self.write_back()?;
        }
        Ok(out)
    }

    /// Write the scene to the scene file (when there is one).
    pub fn write_back(&mut self) -> CmdResult<()> {
        if let Some(p) = self.scene_path.clone() {
            let scene = self.backend.scene()?;
            save_scene(&p, &scene)?;
        }
        Ok(())
    }

    fn request_inner(&mut self, mut req: Value) -> CmdResult<Outcome> {
        if let Some(steps) = req.get("batch").cloned() {
            let mut calls: Vec<Call> = parse(steps)?;
            for c in &mut calls {
                if host::is_host_command(&c.command) {
                    return Err(CommandError::invalid(format!("`{}` cannot run inside a batch", c.command)));
                }
                let (p, write) = host::resolve_paths(&c.command, std::mem::take(&mut c.params))?;
                if write.is_some() {
                    return Err(CommandError::invalid("model.export with `path` cannot run inside a batch"));
                }
                c.params = p;
            }
            return self.backend.request(json!({ "batch": calls }));
        }
        let command = req.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        match command.as_str() {
            STORYBOARD_COMMAND | SEQUENCE_COMMAND | SHEET_COMMAND => {
                let scene = self.backend.scene()?;
                let (mut n, sequence) = (0usize, command == SEQUENCE_COMMAND);
                let mut render = |p: Value| {
                    n += 1;
                    if sequence && n % 48 == 1 {
                        eprintln!("rendering picture {n}");
                    }
                    self.snapshot(p).map(|(_, png)| png)
                };
                let r = host::render_with(&command, &scene, params, &mut render).expect("a render command")?;
                self.query(r)
            }
            "scene.save" => {
                let p: SaveParams = parse(params)?;
                let path = p
                    .path
                    .map(PathBuf::from)
                    .or_else(|| self.scene_path.clone())
                    .ok_or_else(|| CommandError::invalid("give `path` (no scene file is open)"))?;
                let scene = self.backend.scene()?;
                save_scene(&path, &scene)?;
                self.query(json!({ "saved": path.display().to_string(), "entities": scene.entities.len() }))
            }
            "scene.open" => {
                let p: OpenParams = parse(params)?;
                let scene = read_scene(Path::new(&p.path))?;
                let n = scene.entities.len();
                let mut out = self.backend.load(scene)?;
                out.result = json!({ "opened": p.path, "entities": n });
                Ok(out)
            }
            _ => {
                let (p, write_to) = host::resolve_paths(&command, params)?;
                if let Some(o) = req.as_object_mut() {
                    o.insert("params".into(), p);
                }
                let mut out = self.backend.request(req)?;
                if let Some(path) = write_to {
                    out.result = host::write_export(&path, out.result)?;
                }
                Ok(out)
            }
        }
    }

    fn query(&mut self, result: Value) -> CmdResult<Outcome> {
        // queries do not move the revision; ask the backend for it cheaply
        let rev = self.backend.request(json!({"command": "history.info"})).map(|o| o.revision).unwrap_or(0);
        Ok(Outcome { result, revision: rev, changed: false })
    }

    /// Render one snapshot through the backend: (metadata, PNG).
    pub fn snapshot(&mut self, params: Value) -> CmdResult<(Value, Vec<u8>)> {
        let out = self.backend.request(json!({ "command": SNAPSHOT_COMMAND, "params": params }))?;
        let png = png_of(&out.result)?;
        let mut meta = out.result;
        if let Some(o) = meta.as_object_mut() {
            o.remove("data");
        }
        Ok((meta, png))
    }

    /// Render the picture of every row of a shot sheet.
    pub fn shot_sheet(&mut self, params: Value) -> CmdResult<(shotstage_commands::sheet::SheetPlan, Vec<Vec<u8>>)> {
        let scene = self.backend.scene()?;
        let plan = shotstage_commands::sheet::sheet_plan(&scene, params)?;
        let mut pngs = Vec::with_capacity(plan.pictures.len());
        for p in &plan.pictures {
            pngs.push(self.snapshot(p.clone())?.1);
        }
        Ok((plan, pngs))
    }

    /// Render every picture of a storyboard.
    pub fn storyboard(&mut self, params: Value) -> CmdResult<Board> {
        let scene = self.backend.scene()?;
        let plan = host::storyboard_plan(&scene, params)?;
        let mut pngs = Vec::with_capacity(plan.pictures.len());
        for p in &plan.pictures {
            pngs.push(self.snapshot(p.snapshot.clone())?.1);
        }
        Ok(Board { plan, pngs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(Box::new(Local::default()), None, false)
    }

    #[test]
    fn every_catalog_command_is_handled() {
        // A bare session answers `unsupported` for host commands; this host must not.
        let mut h = host();
        h.call("scene.template", json!({"name": "packshot"})).unwrap();
        for c in Session::default().list() {
            if c.id == "scene.open" || c.id.starts_with("render.") {
                continue; // need a file / a GPU: covered below
            }
            if let Err(e) = h.call(&c.id, json!({})) {
                assert_ne!(e.code, "unsupported", "{}: {}", c.id, e.message);
            }
        }
    }

    #[test]
    fn files_round_trip() {
        let dir = std::env::temp_dir().join(format!("shotstage-cli-host-{}", std::process::id()));
        let scene = dir.join("a.shotstage.json");
        let mut h = Host::new(Box::new(Local::default()), Some(scene.clone()), true);
        h.call("entity.create", json!({"type": "camera", "id": "cam"})).unwrap();
        assert!(read_scene(&scene).unwrap().contains("cam"), "autosaved");
        let other = dir.join("b.json");
        h.call("scene.save", json!({"path": other.to_str().unwrap()})).unwrap();
        h.call("scene.new", json!({})).unwrap();
        let o = h.call("scene.open", json!({"path": other.to_str().unwrap()})).unwrap();
        assert!(o.changed);
        let s = h.backend.scene().unwrap();
        assert!(s.contains("cam"));
        h.call("history.undo", json!({})).unwrap();
        assert!(!h.backend.scene().unwrap().contains("cam"), "open is one undo step");
        assert_eq!(h.call("scene.open", json!({"path": "/nope.json"})).unwrap_err().code, "failed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_state_drives_snapshot_frame() {
        let mut h = host();
        h.call("scene.template", json!({"name": "packshot"})).unwrap();
        h.call("editor.setFrame", json!({"frame": 30})).unwrap();
        let st = h.call("editor.state", json!({})).unwrap().result;
        assert_eq!(st["frame"], 30.0);
        assert!(h.request(json!({"batch": [{"command": "render.snapshot"}]})).is_err());
    }
}
