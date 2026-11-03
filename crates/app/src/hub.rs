//! The shared session behind one editor instance. The egui UI and the JavaScript API both go
//! through [`dispatch`], so every change (from a click or from an agent) reaches the same
//! listeners and the same undo history.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};
use shotstage_commands::{CommandError, Outcome, Session};

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
