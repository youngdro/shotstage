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
}

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
        let view = View { view: plan.view, proj: plan.proj, depth_near: 0.0, depth_far: 0.0 };
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

/// Editor-level commands that act on the view rather than the document.
fn editor_command(hub: &mut Hub, request: &Value) -> Option<Result<Outcome, CommandError>> {
    let cmd = request.get("command")?.as_str()?;
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let rev = hub.session.revision();
    let ok = |result| Some(Ok(Outcome { result, revision: rev, changed: false }));
    match cmd {
        "editor.select" => {
            let id = params.get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(i) = &id
                && !hub.session.scene().contains(i)
            {
                return Some(Err(CommandError::new("failed", format!("no entity with id `{i}`"))));
            }
            hub.selected = id;
            ok(Value::Null)
        }
        "editor.setFrame" => match params.get("frame").and_then(Value::as_f64) {
            Some(f) if f.is_finite() => {
                hub.frame = f as f32;
                ok(Value::Null)
            }
            _ => Some(Err(CommandError::invalid("`frame` must be a number"))),
        },
        "editor.setView" => {
            if let Some(p) = params.get("pass") {
                hub.pass = match p.as_str() {
                    Some("clay") => shotstage_render::Pass::Clay,
                    Some("depth") => shotstage_render::Pass::Depth,
                    Some("normal") => shotstage_render::Pass::Normal,
                    _ => return Some(Err(CommandError::invalid("`pass` must be clay, depth or normal"))),
                };
            }
            if let Some(l) = params.get("lookThrough") {
                let Some(l) = l.as_bool() else {
                    return Some(Err(CommandError::invalid("`lookThrough` must be a boolean")));
                };
                hub.look_through = l;
            }
            ok(Value::Null)
        }
        "editor.state" => ok(json!({
            "selected": hub.selected, "frame": hub.frame, "lookThrough": hub.look_through,
            "pass": match hub.pass { shotstage_render::Pass::Clay => "clay", shotstage_render::Pass::Depth => "depth", shotstage_render::Pass::Normal => "normal" },
        })),
        _ => None,
    }
}

pub const EDITOR_COMMANDS: &[(&str, &str)] = &[
    ("editor.select", "Select an entity in the editor (params: {id: string|null})."),
    ("editor.setFrame", "Move the editor's playhead (params: {frame: number})."),
    ("editor.setView", "Viewport options (params: {pass?: \"clay\"|\"depth\"|\"normal\", lookThrough?: boolean})."),
    ("editor.state", "The editor's selection, current frame and viewport options."),
];

/// Run a request (`{"command", "params"}` or `{"batch": [...]}`), then notify listeners.
pub fn dispatch(hub: &SharedHub, request: Value) -> Result<Outcome, CommandError> {
    let (result, listeners, ctx) = {
        let mut h = hub.borrow_mut();
        let r = match editor_command(&mut h, &request) {
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
