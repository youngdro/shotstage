//! The shared session behind one editor instance. The egui UI and the JavaScript API both go
//! through [`dispatch`], so every change (from a click or from an agent) reaches the same
//! listeners and the same undo history.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use shotstage_commands::snapshot::{SNAPSHOT_COMMAND, SnapshotPass, SnapshotPlan};
use shotstage_commands::{CommandError, Outcome, Session};
use shotstage_render::{Pass, Renderer, View};

pub type Listener = Rc<dyn Fn(&Value)>;

#[derive(Default)]
pub struct Hub {
    pub session: Session,
    listeners: Vec<(u32, Listener)>,
    next_listener: u32,
    pub(crate) repaint: Option<egui::Context>,
    /// Editor state that agents may read: selection and current frame.
    pub selected: Option<String>,
    pub frame: f32,
    /// Viewport shading pass.
    pub pass: shotstage_render::Pass,
    /// Viewport shows the shot camera instead of the free orbit view.
    pub look_through: bool,
    /// The scene as of a revision, shared by the UI until it changes (no copy per call).
    pub(crate) scene_cache: Option<(u64, Rc<shotstage_core::Scene>)>,
    /// GPU of the editor window, used for snapshots.
    pub(crate) gpu: Option<eframe::egui_wgpu::RenderState>,
    /// Offscreen renderer for snapshots (separate from the viewport's).
    pub(crate) snap_renderer: Option<Renderer>,
    /// Snapshots waiting for the GPU (the UI polls the device while > 0).
    pub(crate) pending_snapshots: u32,
    /// Mesh edit mode: the mesh being edited and its selected vertices / edges / faces.
    pub edit: Option<EditState>,
    /// The UV editor window: the mesh shown and its selected face corners.
    pub uv: Option<UvState>,
}

pub use shotstage_commands::host::{EditState, SelectMode, UvSelectMode, UvState, editable};

pub(crate) fn render_pass(p: SnapshotPass) -> Pass {
    match p {
        SnapshotPass::Clay => Pass::Clay,
        SnapshotPass::Depth => Pass::Depth,
        SnapshotPass::Normal => Pass::Normal,
    }
}

impl Hub {
    /// Plan a snapshot and draw it into the snapshot renderer; returns what to read back.
    pub(crate) fn draw_snapshot(
        &mut self,
        params: Value,
    ) -> Result<(SnapshotPlan, eframe::egui_wgpu::RenderState, shotstage_render::Readback), CommandError> {
        let plan = shotstage_commands::snapshot::plan(self.session.scene(), params, Some(self.frame))?;
        let rs = self.gpu.clone().ok_or_else(|| CommandError::new("unsupported", "the editor has no GPU"))?;
        let renderer = self.snap_renderer.get_or_insert_with(|| Renderer::new(&rs.device));
        let [depth_near, depth_far] = plan.depth_range.unwrap_or([0.0, 0.0]);
        let view = View { view: plan.view, proj: plan.proj, depth_near, depth_far };
        renderer.render(
            &rs.device,
            &rs.queue,
            self.session.scene(),
            plan.frame,
            view,
            render_pass(plan.pass),
            &shotstage_render::clean_overlay(&plan.camera),
            (plan.width, plan.height),
        );
        let rb = renderer
            .readback(&rs.device, &rs.queue)
            .ok_or_else(|| CommandError::new("failed", "nothing was rendered"))?;
        Ok((plan, rs, rb))
    }
}

pub type SharedHub = Rc<RefCell<Hub>>;

impl Hub {
    pub fn new(session: Session) -> SharedHub {
        Rc::new(RefCell::new(Hub { session, ..Default::default() }))
    }

    pub fn subscribe(&mut self, f: Listener) -> u32 {
        self.next_listener += 1;
        self.listeners.push((self.next_listener, f));
        self.next_listener
    }

    pub fn unsubscribe(&mut self, id: u32) -> bool {
        let n = self.listeners.len();
        self.listeners.retain(|(i, _)| *i != id);
        self.listeners.len() != n
    }
}

/// `editor.*` commands: the shared view logic (`shotstage_commands::host`) applied to this
/// editor's selection, playhead and viewport.
fn editor_command(hub: &mut Hub, request: &Value) -> Option<Result<Outcome, CommandError>> {
    use shotstage_commands::host::{ViewState, view_command};
    let cmd = request.get("command")?.as_str()?;
    if !cmd.starts_with("editor.") {
        return None;
    }
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let mut view = ViewState {
        selected: hub.selected.clone(),
        frame: Some(hub.frame),
        pass: match hub.pass {
            Pass::Clay => SnapshotPass::Clay,
            Pass::Depth => SnapshotPass::Depth,
            Pass::Normal => SnapshotPass::Normal,
        },
        look_through: hub.look_through,
        edit: hub.edit.clone(),
        uv: hub.uv.clone(),
    };
    let r = view_command(&mut view, hub.session.scene(), cmd, params)?;
    hub.edit = view.edit;
    hub.uv = view.uv;
    hub.selected = view.selected;
    hub.frame = view.frame.unwrap_or(hub.frame);
    hub.pass = render_pass(view.pass);
    hub.look_through = view.look_through;
    let revision = hub.session.revision();
    Some(r.map(|result| Outcome { result, revision, changed: false }))
}

/// Run a request (`{"command", "params"}` or `{"batch": [...]}`), then notify listeners.
pub fn dispatch(hub: &SharedHub, request: Value) -> Result<Outcome, CommandError> {
    let (result, listeners, ctx) = {
        let mut h = hub.borrow_mut();
        let r = match editor_command(&mut h, &request).or_else(|| host_command(&mut h, &request)) {
            Some(r) => r,
            None if request.get("command").and_then(Value::as_str) == Some(SNAPSHOT_COMMAND) => {
                snapshot_sync(&mut h, &request)
            }
            None => h.session.dispatch_json(request.clone()),
        };
        let listeners: Vec<Listener> = h.listeners.iter().map(|(_, f)| f.clone()).collect();
        (r, listeners, h.repaint.clone())
    };
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
    if let Ok(o) = &result {
        let event =
            json!({ "type": if o.changed { "change" } else { "command" }, "revision": o.revision, "request": request });
        for l in listeners {
            l(&event);
        }
    }
    result
}

/// The other host commands: files and the rendering ones that take many pictures.
fn host_command(h: &mut Hub, request: &Value) -> Option<Result<Outcome, CommandError>> {
    use shotstage_commands::host::{self, SEQUENCE_COMMAND, STORYBOARD_COMMAND};
    let cmd = request.get("command")?.as_str()?;
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let rev = h.session.revision();
    let done = |result: Value| Ok(Outcome { result, revision: rev, changed: false });
    let r = match cmd {
        "scene.save" => (|| {
            let p: host::SaveParams = shotstage_commands::parse(params)?;
            let json = serde_json::to_string_pretty(h.session.scene()).unwrap_or_default();
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(path) = p.path {
                std::fs::write(&path, &json)
                    .map_err(|e| CommandError::new("failed", format!("writing {path}: {e}")))?;
                return done(json!({ "saved": path }));
            }
            let name = p
                .path
                .as_deref()
                .and_then(|n| n.rsplit(['/', '\\']).next())
                .filter(|n| !n.is_empty())
                .unwrap_or("scene.shotstage.json")
                .to_string();
            match crate::files::save(&name, json.as_bytes(), "application/json") {
                Ok(Some(w)) => done(json!({ "saved": w })),
                Ok(None) => Err(CommandError::new("failed", "saving was cancelled")),
                Err(e) => Err(CommandError::new("failed", e)),
            }
        })(),
        #[cfg(not(target_arch = "wasm32"))]
        "scene.open" => (|| {
            let p: host::OpenParams = shotstage_commands::parse(params)?;
            let text = std::fs::read_to_string(&p.path)
                .map_err(|e| CommandError::new("failed", format!("reading {}: {e}", p.path)))?;
            let scene: shotstage_core::Scene =
                serde_json::from_str(&text).map_err(|e| CommandError::invalid(format!("parsing {}: {e}", p.path)))?;
            let n = scene.entities.len();
            h.session.replace_scene(scene);
            Ok(Outcome {
                result: json!({ "opened": p.path, "entities": n }),
                revision: h.session.revision(),
                changed: true,
            })
        })(),
        #[cfg(not(target_arch = "wasm32"))]
        STORYBOARD_COMMAND | SEQUENCE_COMMAND => {
            let scene = h.session.scene().clone();
            let mut render = |p: Value| {
                let o = snapshot_sync(h, &json!({ "params": p }))?;
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(o.result["data"].as_str().unwrap_or_default())
                    .map_err(|e| CommandError::new("failed", e.to_string()))
            };
            host::render_with(cmd, &scene, params, &mut render)?.and_then(done)
        }
        #[cfg(target_arch = "wasm32")]
        STORYBOARD_COMMAND | SEQUENCE_COMMAND => Err(CommandError::new(
            "unsupported",
            format!("in the browser {cmd} is asynchronous: use `await editor.call(\"{cmd}\", params)`"),
        )),
        // file paths (desktop): read them in, and write exports out
        #[cfg(not(target_arch = "wasm32"))]
        "model.import" | "image.import" | "environment.import" | "model.export" if params.get("path").is_some() => {
            (|| {
                let (p, write_to) = host::resolve_paths(cmd, params)?;
                let mut req = request.clone();
                req["params"] = p;
                let mut out = h.session.dispatch_json(req)?;
                if let Some(path) = write_to {
                    out.result = host::write_export(&path, out.result)?;
                }
                Ok(out)
            })()
        }
        _ => return None,
    };
    Some(r)
}

#[cfg(not(target_arch = "wasm32"))]
fn snapshot_sync(h: &mut Hub, request: &Value) -> Result<Outcome, CommandError> {
    let (plan, rs, rb) = h.draw_snapshot(request.get("params").cloned().unwrap_or(json!({})))?;
    let (tx, rx) = std::sync::mpsc::channel();
    rb.buffer.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    let fail = |e: String| CommandError::new("failed", e);
    rs.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| fail(e.to_string()))?;
    rx.recv().map_err(|e| fail(e.to_string()))?.map_err(|e| fail(e.to_string()))?;
    let png = shotstage_render::encode_png(&rb.rgba().map_err(fail)?, rb.width, rb.height).map_err(fail)?;
    let result = shotstage_commands::snapshot::result_json(&plan, shotstage_render::base64(&png));
    Ok(Outcome { result, revision: h.session.revision(), changed: false })
}

#[cfg(target_arch = "wasm32")]
fn snapshot_sync(_: &mut Hub, _: &Value) -> Result<Outcome, CommandError> {
    Err(CommandError::new(
        "unsupported",
        "in the browser render.snapshot is asynchronous: use `await editor.snapshot(params)`",
    ))
}

/// Convenience for the UI: run one command and log failures instead of returning them.
pub fn run(hub: &SharedHub, command: &str, params: Value) -> Option<Value> {
    match dispatch(hub, json!({ "command": command, "params": params })) {
        Ok(o) => Some(o.result),
        Err(e) => {
            log::warn!("{command}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The editor runs the whole catalog: no command falls through to the bare session's
    /// "host command" answer (the GPU ones fail here only because a test has no window).
    #[test]
    fn editor_handles_every_command() {
        let hub = Hub::new(Session::default());
        dispatch(&hub, json!({"command": "scene.template", "params": {"name": "packshot"}})).unwrap();
        let list = hub.borrow().session.list();
        for c in list {
            if c.id == "scene.save" {
                continue; // opens a save dialog on desktop
            }
            if let Err(e) = dispatch(&hub, json!({"command": c.id, "params": {}})) {
                assert!(
                    !e.message.contains("is a host command"),
                    "{} is not handled by the editor: {}",
                    c.id,
                    e.message
                );
            }
        }
    }

    #[test]
    fn editor_commands_change_the_view_and_files_load() {
        let hub = Hub::new(Session::default());
        dispatch(&hub, json!({"command": "entity.create", "params": {"type": "camera", "id": "cam"}})).unwrap();
        dispatch(&hub, json!({"command": "editor.select", "params": {"id": "cam"}})).unwrap();
        dispatch(&hub, json!({"command": "editor.setFrame", "params": {"frame": 12}})).unwrap();
        dispatch(&hub, json!({"command": "editor.setView", "params": {"pass": "depth", "lookThrough": true}})).unwrap();
        {
            let h = hub.borrow();
            assert_eq!((h.selected.as_deref(), h.frame, h.look_through), (Some("cam"), 12.0, true));
            assert!(matches!(h.pass, Pass::Depth));
        }
        let dir = std::env::temp_dir().join(format!("shotstage-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.json");
        let path = file.to_str().unwrap();
        dispatch(&hub, json!({"command": "scene.save", "params": {"path": path}})).unwrap();
        dispatch(&hub, json!({"command": "scene.new"})).unwrap();
        let o = dispatch(&hub, json!({"command": "scene.open", "params": {"path": path}})).unwrap();
        assert!(o.changed && hub.borrow().session.scene().contains("cam"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
