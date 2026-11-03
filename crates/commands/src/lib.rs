//! The command registry: every edit in shotstage is a named command with JSON parameters.
//!
//! The egui editor, the browser API (`window.shotstage`), the CLI and the MCP server all call
//! [`Session::dispatch`] / [`Session::batch`], so they can do exactly the same things.
//! Parameters are untrusted: bad input returns a [`CommandError`], never a panic.

pub mod arrange;
#[cfg(not(target_arch = "wasm32"))]
pub mod blender;
mod builtin;
pub mod clipboard;
pub mod images;
pub mod lights;
pub mod model;
pub mod motion;
mod placement;
pub mod prefabs;
pub mod rig;
pub mod snapshot;
pub mod templates;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shotstage_core::{CoreError, Scene};

pub use builtin::registry;

/// Error returned to callers. `code` is stable for programs; `message` is for people and agents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CommandError {
    /// `unknown_command` | `invalid_params` | `failed` | `nothing_to_undo` | `nothing_to_redo`
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_params", message)
    }
}

impl From<CoreError> for CommandError {
    fn from(e: CoreError) -> Self {
        let code = match e {
            CoreError::Invalid(_) => "invalid_params",
            _ => "failed",
        };
        Self::new(code, e.to_string())
    }
}

pub type CmdResult<T> = Result<T, CommandError>;

type Run = fn(&mut Scene, Value) -> CmdResult<Value>;

/// One registered command.
pub struct Command {
    pub id: &'static str,
    pub summary: &'static str,
    /// Changes the scene (recorded in undo history). Queries are `false`.
    pub mutates: bool,
    pub schema: fn() -> Value,
    pub run: Run,
}

/// Public description of a command, as returned by `command.list`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CommandInfo {
    pub id: String,
    pub summary: String,
    pub mutates: bool,
    /// JSON Schema of the `params` object.
    pub params: Value,
}

/// Parse `params` into a typed struct, mapping serde errors to `invalid_params`.
pub fn parse<P: DeserializeOwned>(params: Value) -> CmdResult<P> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| CommandError::invalid(e.to_string()))
}

pub fn schema_of<P: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(P)).unwrap_or(Value::Null)
}

/// Declare a command from an id, summary, mutating flag, params type and handler.
#[macro_export]
macro_rules! command {
    ($id:literal, $summary:literal, mutates: $m:expr, $params:ty, $handler:expr) => {
        $crate::Command {
            id: $id,
            summary: $summary,
            mutates: $m,
            schema: $crate::schema_of::<$params>,
            run: |scene, v| {
                let p: $params = $crate::parse(v)?;
                let f: fn(&mut shotstage_core::Scene, $params) -> $crate::CmdResult<serde_json::Value> = $handler;
                f(scene, p)
            },
        }
    };
}

/// One step of a batch.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Call {
    /// Command id, e.g. `entity.create`.
    pub command: String,
    #[serde(default)]
    pub params: Value,
}

/// What a dispatch returns on success.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub result: Value,
    /// Increments whenever the scene changes; UIs redraw when it moves.
    pub revision: u64,
    pub changed: bool,
}

const HISTORY_LIMIT: usize = 200;
pub const BATCH_LIMIT: usize = 500;

/// Commands handled by the session itself (they need the history, not just the scene).
const SESSION_COMMANDS: &[(&str, &str, bool)] = &[
    ("command.list", "List every command with its JSON Schema.", false),
    ("history.undo", "Undo the last change (a batch undoes as one step).", true),
    ("history.redo", "Redo the last undone change.", true),
    ("history.info", "How many undo and redo steps are available.", false),
];

/// An open document: the scene plus undo/redo history.
pub struct Session {
    scene: Scene,
    undo: Vec<(String, Scene)>,
    redo: Vec<(String, Scene)>,
    revision: u64,
    commands: Vec<Command>,
    /// Merge key of the last recorded change (see [`Session::dispatch_merged`]).
    last_merge: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new(Scene::default())
    }
}

impl Session {
    pub fn new(scene: Scene) -> Self {
        Self { scene, undo: Vec::new(), redo: Vec::new(), revision: 0, commands: registry(), last_merge: None }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn list(&self) -> Vec<CommandInfo> {
        let empty = || json!({"type": "object", "properties": {}});
        SESSION_COMMANDS
            .iter()
            .map(|(id, summary, mutates)| CommandInfo {
                id: id.to_string(),
                summary: summary.to_string(),
                mutates: *mutates,
                params: empty(),
            })
            .chain(self.commands.iter().map(|c| CommandInfo {
                id: c.id.into(),
                summary: c.summary.into(),
                mutates: c.mutates,
                params: (c.schema)(),
            }))
            .collect()
    }

    fn find(&self, id: &str) -> CmdResult<&Command> {
        self.commands.iter().find(|c| c.id == id).ok_or_else(|| {
            CommandError::new("unknown_command", format!("no command `{id}`; call command.list for the catalog"))
        })
    }

    fn push_undo(&mut self, label: &str, before: Scene) {
        self.last_merge = None;
        self.undo.push((label.to_string(), before));
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Run one command. A failing command leaves the scene unchanged.
    pub fn dispatch(&mut self, id: &str, params: Value) -> CmdResult<Outcome> {
        self.dispatch_merged(id, params, None)
    }

    /// Like [`Session::dispatch`], but consecutive changes with the same `merge` key share one
    /// undo step. A UI uses this for a drag (one key per gesture); agents can use it to stream.
    pub fn dispatch_merged(&mut self, id: &str, params: Value, merge: Option<&str>) -> CmdResult<Outcome> {
        match id {
            "command.list" => return Ok(self.outcome(json!(self.list()), false)),
            "history.info" => {
                let r = json!({
                    "undo": self.undo.iter().rev().map(|(l, _)| l).collect::<Vec<_>>(),
                    "redo": self.redo.iter().rev().map(|(l, _)| l).collect::<Vec<_>>(),
                });
                return Ok(self.outcome(r, false));
            }
            "history.undo" => {
                let (label, prev) =
                    self.undo.pop().ok_or_else(|| CommandError::new("nothing_to_undo", "nothing to undo"))?;
                let cur = std::mem::replace(&mut self.scene, prev);
                self.redo.push((label.clone(), cur));
                return Ok(self.outcome(json!({ "undone": label }), true));
            }
            "history.redo" => {
                let (label, next) =
                    self.redo.pop().ok_or_else(|| CommandError::new("nothing_to_redo", "nothing to redo"))?;
                let cur = std::mem::replace(&mut self.scene, next);
                self.undo.push((label.clone(), cur));
                return Ok(self.outcome(json!({ "redone": label }), true));
            }
            _ => {}
        }
        let cmd = self.find(id)?;
        let (run, mutates) = (cmd.run, cmd.mutates);
        if !mutates {
            let mut scratch = self.scene.clone();
            let r = run(&mut scratch, params)?;
            return Ok(self.outcome(r, false));
        }
        let before = self.scene.clone();
        match run(&mut self.scene, params) {
            Ok(r) => {
                let changed = self.scene != before;
                if changed {
                    let merging = merge.is_some() && merge == self.last_merge.as_deref() && !self.undo.is_empty();
                    if merging {
                        self.redo.clear();
                    } else {
                        self.push_undo(id, before);
                        self.last_merge = merge.map(str::to_string);
                    }
                }
                Ok(self.outcome(r, changed))
            }
            Err(e) => {
                self.scene = before;
                Err(e)
            }
        }
    }

    /// Run several commands as one transaction: all succeed (one undo step) or none apply.
    /// Returns each step's result.
    pub fn batch(&mut self, calls: Vec<Call>) -> CmdResult<Outcome> {
        if calls.len() > BATCH_LIMIT {
            return Err(CommandError::invalid(format!("batch has {} steps (limit {BATCH_LIMIT})", calls.len())));
        }
        let before = self.scene.clone();
        let mut results = Vec::with_capacity(calls.len());
        for (i, call) in calls.iter().enumerate() {
            if call.command.starts_with("history.") {
                self.scene = before;
                return Err(CommandError::invalid(format!("step {i}: history commands cannot run inside a batch")));
            }
            let r = if call.command == "command.list" {
                Ok(json!(self.list()))
            } else {
                match self.find(&call.command) {
                    Ok(c) => {
                        let run = c.run;
                        run(&mut self.scene, call.params.clone())
                    }
                    Err(e) => Err(e),
                }
            };
            match r {
                Ok(v) => results.push(v),
                Err(e) => {
                    self.scene = before;
                    return Err(CommandError::new(&e.code, format!("step {i} ({}): {}", call.command, e.message)));
                }
            }
        }
        let changed = self.scene != before;
        if changed {
            self.push_undo("batch", before);
        }
        self.last_merge = None;
        Ok(self.outcome(json!(results), changed))
    }

    /// Run a JSON request: `{"command": "...", "params": {...}, "merge"?: "key"}` or
    /// `{"batch": [...]}`.
    pub fn dispatch_json(&mut self, request: Value) -> CmdResult<Outcome> {
        if let Some(steps) = request.get("batch") {
            let calls: Vec<Call> = parse(steps.clone())?;
            return self.batch(calls);
        }
        let merge = request.get("merge").and_then(Value::as_str).map(str::to_string);
        let mut request = request;
        if let Some(o) = request.as_object_mut() {
            o.remove("merge");
        }
        let call: Call = parse(request)?;
        self.dispatch_merged(&call.command, call.params, merge.as_deref())
    }

    /// Replace the scene (e.g. open a file) as an undoable step.
    pub fn replace_scene(&mut self, scene: Scene) {
        let before = std::mem::replace(&mut self.scene, scene);
        self.push_undo("scene.load", before);
        self.revision += 1;
    }

    fn outcome(&mut self, result: Value, changed: bool) -> Outcome {
        if changed {
            self.revision += 1;
        }
        Outcome { result, revision: self.revision, changed }
    }
}

#[cfg(test)]
mod tests;
