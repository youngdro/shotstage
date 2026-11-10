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
pub mod environment;
pub mod host;
pub mod humans;
pub mod images;
pub mod lights;
pub mod meshedit;
pub mod model;
pub mod motion;
mod placement;
pub mod prefabs;
pub mod rig;
pub mod sheet;
pub mod snapshot;
pub mod templates;
pub mod texture;

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
    /// Host commands only: what running it takes (`gpu`, `files`, `view`). Every entry point
    /// (browser, CLI, MCP) lists the same catalog and runs these with what it has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
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
/// Memory the undo history may hold beyond the scene itself: steps that changed big meshes
/// keep their own copy of them, so a long session of mesh edits on a heavy model would
/// otherwise grow without bound. The oldest steps go first.
const HISTORY_BUDGET: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 768 << 20 };
/// Steps kept whatever they cost.
const HISTORY_MIN_STEPS: usize = 5;
pub const BATCH_LIMIT: usize = 500;

/// Commands handled by the session itself (they need the history, not just the scene).
const SESSION_COMMANDS: &[(&str, &str, bool)] = &[
    ("command.list", "List every command with its JSON Schema.", false),
    ("history.undo", "Undo the last change (a batch undoes as one step).", true),
    ("history.redo", "Redo the last undone change.", true),
    ("history.info", "How many undo and redo steps are available.", false),
    (
        "history.discard",
        "Undo the last change and forget it (no redo): for previews that are tried and thrown away. \
         With `merge`, only when the last change was made under that merge key (a cancelled drag \
         that never moved anything leaves the step before it alone).",
        true,
    ),
];

/// One undo (or redo) step: the scene to go back to, and roughly how much memory only it holds.
struct Step {
    label: String,
    scene: Scene,
    bytes: usize,
}

impl Step {
    /// `scene`, kept next to `neighbour` (the state it leads to or comes from): what they
    /// share costs nothing extra.
    fn new(label: &str, scene: Scene, neighbour: &Scene) -> Self {
        let bytes = scene.bytes_not_shared_with(neighbour);
        Self { label: label.to_string(), scene, bytes }
    }
}

/// An open document: the scene plus undo/redo history.
pub struct Session {
    scene: Scene,
    undo: Vec<Step>,
    redo: Vec<Step>,
    revision: u64,
    commands: Vec<Command>,
    /// Merge key of the last recorded change (see [`Session::dispatch_merged`]).
    last_merge: Option<String>,
    /// See [`HISTORY_BUDGET`].
    history_budget: usize,
}

impl Default for Session {
    fn default() -> Self {
        Self::new(Scene::default())
    }
}

impl Session {
    pub fn new(scene: Scene) -> Self {
        Self {
            scene,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            commands: registry(),
            last_merge: None,
            history_budget: HISTORY_BUDGET,
        }
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
                params: if *id == "history.discard" {
                    json!({"type": "object", "properties": {"merge": {
                        "type": "string",
                        "description": "Only discard when the last change was made under this merge key."
                    }}})
                } else {
                    empty()
                },
                needs: vec![],
            })
            .chain(self.commands.iter().map(|c| CommandInfo {
                id: c.id.into(),
                summary: c.summary.into(),
                mutates: c.mutates,
                params: (c.schema)(),
                needs: vec![],
            }))
            .chain(host::infos())
            .collect()
    }

    fn find(&self, id: &str) -> CmdResult<&Command> {
        if host::is_host_command(id) {
            return Err(host::unsupported(id));
        }
        self.commands.iter().find(|c| c.id == id).ok_or_else(|| {
            CommandError::new("unknown_command", format!("no command `{id}`; call command.list for the catalog"))
        })
    }

    fn push_undo(&mut self, label: &str, before: Scene) {
        self.last_merge = None;
        self.undo.push(Step::new(label, before, &self.scene));
        self.redo.clear();
        self.trim_history();
    }

    /// Drop the oldest steps past the step limit or the memory budget.
    fn trim_history(&mut self) {
        let mut total: usize = self.undo.iter().chain(&self.redo).map(|s| s.bytes).sum();
        let mut drop = 0;
        while self.undo.len() - drop > HISTORY_MIN_STEPS
            && (self.undo.len() - drop > HISTORY_LIMIT || total > self.history_budget)
        {
            total -= self.undo[drop].bytes;
            drop += 1;
        }
        self.undo.drain(..drop);
    }

    /// Change how much memory the undo history may hold (bytes); the oldest steps beyond it go.
    pub fn set_history_budget(&mut self, bytes: usize) {
        self.history_budget = bytes;
        self.trim_history();
    }

    /// Memory the history holds beyond the scene itself, roughly (bytes).
    pub fn history_bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(|s| s.bytes).sum()
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
                    "undo": self.undo.iter().rev().map(|s| &s.label).collect::<Vec<_>>(),
                    "redo": self.redo.iter().rev().map(|s| &s.label).collect::<Vec<_>>(),
                    "memoryMb": (self.history_bytes() as f64 / 1048576.0 * 10.0).round() / 10.0,
                });
                return Ok(self.outcome(r, false));
            }
            "history.undo" => {
                let step = self.undo.pop().ok_or_else(|| CommandError::new("nothing_to_undo", "nothing to undo"))?;
                let cur = std::mem::replace(&mut self.scene, step.scene);
                self.last_merge = None;
                self.redo.push(Step::new(&step.label, cur, &self.scene));
                return Ok(self.outcome(json!({ "undone": step.label }), true));
            }
            "history.discard" => {
                if let Some(key) = params.get("merge").and_then(Value::as_str)
                    && self.last_merge.as_deref() != Some(key)
                {
                    return Ok(self.outcome(json!({ "discarded": null }), false));
                }
                let step = self.undo.pop().ok_or_else(|| CommandError::new("nothing_to_undo", "nothing to undo"))?;
                self.scene = step.scene;
                self.last_merge = None;
                return Ok(self.outcome(json!({ "discarded": step.label }), true));
            }
            "history.redo" => {
                let step = self.redo.pop().ok_or_else(|| CommandError::new("nothing_to_redo", "nothing to redo"))?;
                let cur = std::mem::replace(&mut self.scene, step.scene);
                self.last_merge = None;
                self.undo.push(Step::new(&step.label, cur, &self.scene));
                return Ok(self.outcome(json!({ "redone": step.label }), true));
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
            // named after its first step, so History reads "Delete" rather than "batch"
            let label = calls.first().map(|c| c.command.as_str()).unwrap_or("batch");
            self.push_undo(label, before);
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
