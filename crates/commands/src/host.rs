//! Host commands: the part of the catalog that needs more than the scene document.
//!
//! A [`Session`](crate::Session) only owns the document. Rendering needs a GPU, reading and
//! writing files needs a file system, and selection / playhead belong to an editor view. Those
//! commands are still listed in the one catalog (`command.list`), with the same JSON Schema, so
//! the browser API, the CLI and the MCP server publish the same set; each host runs them with
//! what it has. Everything a host needs to agree on (which frames a storyboard shows, the depth
//! range of a sequence, what `editor.*` does to the view) is computed here, without a GPU.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shotstage_core::Scene;
use shotstage_core::depthfit::DepthMap;

use crate::builtin::{NoParams, frame_ok};
use crate::snapshot::{SNAPSHOT_COMMAND, SnapshotParams, SnapshotPass};
use crate::{CmdResult, CommandError, CommandInfo, parse, schema_of};

/// What a host command needs.
pub const GPU: &str = "gpu";
pub const FILES: &str = "files";
pub const VIEW: &str = "view";

pub struct HostCommand {
    pub id: &'static str,
    pub summary: &'static str,
    pub needs: &'static [&'static str],
    pub schema: fn() -> Value,
}

pub const STORYBOARD_COMMAND: &str = "render.storyboard";
pub const SEQUENCE_COMMAND: &str = "render.sequence";

/// Every host command. Hosts must handle each one (or answer `unsupported` with a hint).
pub fn host_commands() -> Vec<HostCommand> {
    vec![
        HostCommand {
            id: SNAPSHOT_COMMAND,
            summary: "Render the view through a camera to a PNG (clay, depth or normal pass) so you can see the shot. Returns {mimeType, data (base64), width, height, camera, frame, pass}.",
            needs: &[GPU],
            schema: schema_of::<SnapshotParams>,
        },
        HostCommand {
            id: STORYBOARD_COMMAND,
            summary: "Render a storyboard: the first / middle / last frame of every shot (clay, optionally depth / normal), with shot descriptions and per-frame camera data. With `out` writes PNGs, storyboard.json and index.html to that folder; in the browser the pictures come back as data URLs with a self-contained `html` page.",
            needs: &[GPU],
            schema: schema_of::<StoryboardParams>,
        },
        HostCommand {
            id: crate::sheet::SHEET_COMMAND,
            summary: "Export a shot sheet for the crew: every shot's picture with its number, name, camera, shot size and angle, focal length, camera move (worked out from the animation), duration and notes. A table or storyboard grid, as a PDF (A4 landscape) or one PNG, labels in English or Chinese. With `out` writes the file; otherwise returns it as base64 `data` with `fileName` and `mimeType`. The analysed rows come back as `shots`.",
            needs: &[GPU],
            schema: schema_of::<crate::sheet::ShotSheetParams>,
        },
        HostCommand {
            id: SEQUENCE_COMMAND,
            summary: "Render every frame of a range through the shot cameras (clay / depth / normal; depth uses one range for the whole sequence so it does not flicker) plus per-frame camera data, as control sequences for video models. With `out` writes numbered PNGs, camera.json and, with `video`, MP4s (ffmpeg); in the browser `video` returns MP4s (base64) instead of the frames.",
            needs: &[GPU],
            schema: schema_of::<SequenceParams>,
        },
        HostCommand {
            id: "scene.save",
            summary: "Save the scene document as JSON: to `path`, else the file the CLI / MCP server was opened with; in the browser it downloads a .shotstage.json file.",
            needs: &[FILES],
            schema: schema_of::<SaveParams>,
        },
        HostCommand {
            id: "scene.open",
            summary: "Open a saved scene file from disk (one undo step). In the browser use scene.load with the document instead.",
            needs: &[FILES],
            schema: schema_of::<OpenParams>,
        },
        HostCommand {
            id: "editor.select",
            summary: "Select an entity in the editor (null clears). Snapshots and the UI follow the selection.",
            needs: &[VIEW],
            schema: schema_of::<SelectParams>,
        },
        HostCommand {
            id: "editor.setFrame",
            summary: "Move the playhead. render.snapshot without `frame` renders this frame.",
            needs: &[VIEW],
            schema: schema_of::<SetFrameParams>,
        },
        HostCommand {
            id: "editor.setView",
            summary: "Viewport options: shading pass (clay, depth, normal), whether to look through the shot camera, and the depth pass range (fitted to the subjects in frame unless set).",
            needs: &[VIEW],
            schema: schema_of::<SetViewParams>,
        },
        HostCommand {
            id: "editor.editMesh",
            summary: "Enter mesh edit mode on a mesh (null leaves it), with a select mode (vertex, edge, face). The edit tools (mesh.extrude, mesh.bevel...) take explicit selections, so this is optional for agents; it shows the person what you are editing.",
            needs: &[VIEW],
            schema: schema_of::<EditMeshParams>,
        },
        HostCommand {
            id: "editor.editSelect",
            summary: "Set the edit-mode selection (vertices, edges, faces); the `selection` a mesh tool returns fits here.",
            needs: &[VIEW],
            schema: schema_of::<EditSelectParams>,
        },
        HostCommand {
            id: "editor.uvEditor",
            summary: "Open the UV editor on a mesh (null closes it): its UV map over its picture, to pick and drag UV vertices, edges, faces or islands. Optional select mode and sticky selection.",
            needs: &[VIEW],
            schema: schema_of::<UvEditorParams>,
        },
        HostCommand {
            id: "editor.uvSelect",
            summary: "Set the UV editor's selection: face corners `[face, index]` and / or whole faces (mesh.uvSelect resolves boxes, sticky and islands).",
            needs: &[VIEW],
            schema: schema_of::<UvEditorSelectParams>,
        },
        HostCommand {
            id: "editor.state",
            summary: "The editor's selection, playhead frame and viewport options.",
            needs: &[VIEW],
            schema: schema_of::<NoParams>,
        },
    ]
}

pub fn is_host_command(id: &str) -> bool {
    host_commands().iter().any(|c| c.id == id)
}

pub(crate) fn infos() -> Vec<CommandInfo> {
    host_commands()
        .into_iter()
        .map(|c| CommandInfo {
            id: c.id.into(),
            summary: c.summary.into(),
            mutates: c.id == "scene.open",
            params: (c.schema)(),
            needs: c.needs.iter().map(|s| s.to_string()).collect(),
        })
        .collect()
}

/// What a bare session answers for a host command.
pub(crate) fn unsupported(id: &str) -> CommandError {
    let needs = host_commands().into_iter().find(|c| c.id == id).map(|c| c.needs).unwrap_or(&[]);
    let how = if needs.contains(&GPU) {
        "it renders on a GPU host: the editor (`await editor.call(...)` in the browser), the CLI (`shotstage run`) or the MCP server"
    } else if needs.contains(&FILES) {
        "it uses the file system: run it through the CLI or the MCP server"
    } else {
        "it acts on an editor view: run it through the editor, `shotstage serve` or the MCP server"
    };
    CommandError::new("unsupported", format!("`{id}` is a host command; {how}"))
}

/// Error for a file `path` param reaching a host without file access.
pub fn needs_files(command: &str, field: &str) -> CommandError {
    CommandError::new(
        "unsupported",
        format!(
            "`{command}` with `{field}` reads or writes a file: use the CLI or the MCP server, or pass the contents as `data`"
        ),
    )
}

// ---------------------------------------------------------------- params

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveParams {
    /// File to write. Default: the scene file the CLI / MCP server was started with.
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenParams {
    /// A `.shotstage.json` scene file.
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectParams {
    /// Entity id, or null to clear the selection.
    pub id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetFrameParams {
    pub frame: f32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetViewParams {
    pub pass: Option<SnapshotPass>,
    /// Show the view through the shot camera instead of the free orbit view.
    pub look_through: Option<bool>,
    /// Viewport depth pass: `[near, far]` metres (or `[near, focusNear, focusFar, far]`), or
    /// `[]` to fit it to what is in frame again (the default).
    pub depth_range: Option<Vec<f32>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditMeshParams {
    /// Mesh to edit, or null to leave edit mode.
    pub id: Option<String>,
    /// What clicks pick. Default vertex.
    pub mode: Option<SelectMode>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditSelectParams {
    pub vertices: Option<Vec<u32>>,
    /// Vertex pairs.
    pub edges: Option<Vec<[u32; 2]>>,
    pub faces: Option<Vec<u32>>,
    pub mode: Option<SelectMode>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UvEditorParams {
    /// Mesh whose UV map to show, or null to close the UV editor.
    pub id: Option<String>,
    /// What clicks pick. Default vertex.
    pub mode: Option<UvSelectMode>,
    /// Picking a UV vertex also picks the corners of neighbouring faces at the same spot.
    pub sticky: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UvEditorSelectParams {
    /// Face corners `[face, index in the face]`.
    pub corners: Option<Vec<[u32; 2]>>,
    /// Whole faces (all their corners).
    pub faces: Option<Vec<u32>>,
    pub mode: Option<UvSelectMode>,
}

// ---------------------------------------------------------------- view state

/// Which elements clicks pick in the UV editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UvSelectMode {
    #[default]
    Vertex,
    Edge,
    Face,
    /// Whole flat pieces.
    Island,
}

/// The UV editor: the mesh shown and the selected face corners.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UvState {
    pub id: String,
    pub mode: UvSelectMode,
    pub sticky: bool,
    /// Selected face corners `[face, index]`, sorted.
    pub selection: Vec<[u32; 2]>,
}

impl UvState {
    pub fn json(&self) -> Value {
        json!({ "id": self.id, "mode": self.mode, "sticky": self.sticky, "selection": self.selection })
    }
}

/// Which elements clicks pick in mesh edit mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SelectMode {
    #[default]
    Vertex,
    Edge,
    Face,
}

/// Mesh edit mode: the mesh being edited and its selection.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EditState {
    pub id: String,
    pub mode: SelectMode,
    pub selection: shotstage_core::meshedit::Elements,
}

impl EditState {
    pub fn json(&self) -> Value {
        json!({ "id": self.id, "mode": self.mode, "selection": self.selection })
    }
}

/// Corner count of each face of a mesh (for checking UV selections).
fn uv_mesh_faces(scene: &Scene, id: &str) -> CmdResult<Vec<usize>> {
    let m = crate::meshedit::edit_mesh(scene, id)?;
    Ok(m.faces.iter().map(Vec::len).collect())
}

/// Can `id` be edited in edit mode? (a mesh that is not rigged)
pub fn editable(scene: &Scene, id: &str) -> CmdResult<()> {
    use shotstage_core::{EntityKind, MeshSource};
    match scene.get(id).map(|e| &e.kind) {
        Ok(EntityKind::Mesh { source: MeshSource::Skinned { .. } | MeshSource::Human { .. }, .. }) => {
            Err(CommandError::new("failed", format!("`{id}` is rigged: pose it instead of editing its mesh")))
        }
        Ok(EntityKind::Mesh { .. }) => Ok(()),
        Ok(_) => Err(CommandError::new("failed", format!("`{id}` is not a mesh"))),
        Err(e) => Err(e.into()),
    }
}

/// The editor view: what `editor.*` commands read and change. The browser and desktop editor
/// keep it next to their UI; the CLI / MCP server keep one per session.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewState {
    pub selected: Option<String>,
    /// Playhead. `None` until something sets it (snapshots then use the timeline start).
    pub frame: Option<f32>,
    pub pass: SnapshotPass,
    pub look_through: bool,
    /// Mesh edit mode, if on.
    pub edit: Option<EditState>,
    /// The UV editor, if open.
    pub uv: Option<UvState>,
    /// Viewport depth pass map; `None` fits it to what is in frame.
    pub depth_range: Option<DepthMap>,
}

/// Run an `editor.*` command against a view. `None` when `id` is not one.
pub fn view_command(view: &mut ViewState, scene: &Scene, id: &str, params: Value) -> Option<CmdResult<Value>> {
    let r = match id {
        "editor.select" => parse::<SelectParams>(params).and_then(|p| {
            if let Some(i) = &p.id
                && !scene.contains(i)
            {
                return Err(CommandError::new("failed", format!("no entity with id `{i}`")));
            }
            view.selected = p.id;
            Ok(Value::Null)
        }),
        "editor.setFrame" => parse::<SetFrameParams>(params).and_then(|p| {
            view.frame = Some(frame_ok(p.frame)?);
            Ok(Value::Null)
        }),
        "editor.setView" => parse::<SetViewParams>(params).and_then(|p| {
            let depth = match p.depth_range.as_deref() {
                None => view.depth_range,
                Some([]) => None,
                Some(v) => Some(DepthMap::from_slice(v).ok_or_else(|| {
                    CommandError::invalid(
                        "depthRange must be [], [near, far] or [near, focusNear, focusFar, far], increasing, from 0",
                    )
                })?),
            };
            view.depth_range = depth;
            if let Some(pass) = p.pass {
                view.pass = pass;
            }
            if let Some(l) = p.look_through {
                view.look_through = l;
            }
            Ok(Value::Null)
        }),
        "editor.editMesh" => parse::<EditMeshParams>(params).and_then(|p| {
            match p.id {
                None => view.edit = None,
                Some(id) => {
                    editable(scene, &id)?;
                    view.selected = Some(id.clone());
                    let keep = view.edit.as_ref().filter(|e| e.id == id).map(|e| e.selection.clone());
                    view.edit =
                        Some(EditState { id, mode: p.mode.unwrap_or_default(), selection: keep.unwrap_or_default() });
                }
            }
            Ok(view.edit.as_ref().map(EditState::json).unwrap_or(Value::Null))
        }),
        "editor.editSelect" => parse::<EditSelectParams>(params).and_then(|p| {
            let edit = view
                .edit
                .as_mut()
                .ok_or_else(|| CommandError::new("failed", "not in edit mode: call editor.editMesh first"))?;
            if let Some(m) = p.mode {
                edit.mode = m;
            }
            edit.selection = shotstage_core::meshedit::Elements {
                vertices: p.vertices.unwrap_or_default(),
                edges: p.edges.unwrap_or_default(),
                faces: p.faces.unwrap_or_default(),
            }
            .normalized();
            Ok(edit.json())
        }),
        "editor.uvEditor" => parse::<UvEditorParams>(params).and_then(|p| {
            match p.id {
                None => view.uv = None,
                Some(id) => {
                    editable(scene, &id)?;
                    let keep = view.uv.take().filter(|u| u.id == id);
                    view.uv = Some(UvState {
                        mode: p.mode.or(keep.as_ref().map(|u| u.mode)).unwrap_or_default(),
                        sticky: p.sticky.or(keep.as_ref().map(|u| u.sticky)).unwrap_or(true),
                        selection: keep.map(|u| u.selection).unwrap_or_default(),
                        id,
                    });
                }
            }
            Ok(view.uv.as_ref().map(UvState::json).unwrap_or(Value::Null))
        }),
        "editor.uvSelect" => parse::<UvEditorSelectParams>(params).and_then(|p| {
            let uv = view
                .uv
                .as_mut()
                .ok_or_else(|| CommandError::new("failed", "the UV editor is closed: call editor.uvEditor first"))?;
            let faces = uv_mesh_faces(scene, &uv.id)?;
            let mut sel: std::collections::BTreeSet<[u32; 2]> = Default::default();
            for c in p.corners.unwrap_or_default() {
                if faces.get(c[0] as usize).is_none_or(|&n| c[1] as usize >= n) {
                    return Err(CommandError::invalid(format!("corner {c:?} does not exist")));
                }
                sel.insert(c);
            }
            for f in p.faces.unwrap_or_default() {
                let n =
                    *faces.get(f as usize).ok_or_else(|| CommandError::invalid(format!("face {f} does not exist")))?;
                sel.extend((0..n as u32).map(|k| [f, k]));
            }
            if let Some(m) = p.mode {
                uv.mode = m;
            }
            uv.selection = sel.into_iter().collect();
            Ok(uv.json())
        }),
        "editor.state" => parse::<NoParams>(params).map(|_| {
            json!({
                "uv": view.uv.as_ref().map(UvState::json),
                "edit": view.edit.as_ref().map(EditState::json),
                "selected": view.selected,
                "frame": view.frame.unwrap_or(scene.timeline.start as f32),
                "pass": view.pass,
                "lookThrough": view.look_through,
                "depthRange": view.depth_range.map(|d| d.to_vec()),
            })
        }),
        _ => return None,
    };
    Some(r)
}

// ---------------------------------------------------------------- subject

/// What a shot is of, when nobody said: the person nearest the centre of the frame, else the
/// nearest-to-centre unlocked object (locked things are floors and backdrops). `None` when
/// nothing is in view.
pub fn auto_subject(scene: &Scene, camera: &str, frame: f32) -> Option<String> {
    use shotstage_core::EntityKind;
    use shotstage_core::eval::{camera_data_at, world_at};
    let lens = camera_data_at(scene, camera, frame).ok()?;
    let world = world_at(scene, camera, frame).ok()?;
    let (eye, inv) = (world.w_axis.truncate(), world.inverse());
    let ty = (lens.vfov() / 2.0).tan();
    let tx = ty * lens.aspect;
    let mut best: Option<(bool, f32, String)> = None;
    for e in scene.entities.iter().filter(|e| e.parent.is_none() && e.visible && e.id != camera) {
        if matches!(e.kind, EntityKind::Camera(_) | EntityKind::Light(_)) {
            continue;
        }
        let person = crate::rig::person_root(scene, &e.id).is_ok_and(|r| r == e.id);
        if e.locked && !person {
            continue;
        }
        let Some(b) = shotstage_core::spatial::bounds(scene, &e.id, frame).ok().flatten() else { continue };
        // the box on screen (corners behind the camera left out)
        let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for i in 0..8 {
            let corner = glam::Vec3::new(
                if i & 1 == 0 { b.min.x } else { b.max.x },
                if i & 2 == 0 { b.min.y } else { b.max.y },
                if i & 4 == 0 { b.min.z } else { b.max.z },
            );
            let c = inv.transform_point3(corner);
            if c.z > -0.05 {
                continue;
            }
            let (x, y) = (c.x / -c.z / tx, c.y / -c.z / ty);
            (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
        }
        if x0 > x1 || x1 < -1.0 || x0 > 1.0 || y1 < -1.0 || y0 > 1.0 {
            continue; // not in frame
        }
        // how far the frame centre is from the box: 0 when the box covers it; nearer wins, and
        // distance from the camera breaks ties between things lined up
        let (dx, dy) = ((x0.max(0.0) + x1.min(0.0)).abs(), (y0.max(0.0) + y1.min(0.0)).abs());
        let score = (dx * dx + dy * dy).sqrt() + 0.02 * (b.center() - eye).length();
        let better = match &best {
            None => true,
            Some((bp, bs, _)) => (person && !bp) || (person == *bp && score < *bs),
        };
        if better {
            best = Some((person, score, e.id.clone()));
        }
    }
    best.map(|(_, _, id)| id)
}

// ---------------------------------------------------------------- storyboard

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BoardPosition {
    First,
    Middle,
    Last,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoryboardParams {
    /// Which frames of each shot. Default first, middle and last (first and last are the
    /// start / end frame conditions of image-to-video models).
    pub positions: Option<Vec<BoardPosition>>,
    /// Passes per frame. Default `["clay"]`.
    pub passes: Option<Vec<SnapshotPass>>,
    /// Picture width in pixels (16..=4096). Default 960.
    pub width: Option<u32>,
    /// Only these shots (0-based indices). Default every shot; with no shots, the whole timeline
    /// through the active camera.
    pub shots: Option<Vec<usize>>,
    /// Folder to write the pictures, storyboard.json and index.html into (CLI / MCP). MCP
    /// defaults to `<scene file>.storyboard/` next to the scene.
    pub out: Option<String>,
    /// Page title. Default "Storyboard".
    pub title: Option<String>,
}

/// One picture to render: where it goes in the board and the `render.snapshot` params.
#[derive(Clone, Debug, Serialize)]
pub struct BoardShotPicture {
    pub shot: usize,
    pub frame_index: usize,
    pub pass: SnapshotPass,
    /// File name used when the board is written to a folder.
    pub file: String,
    pub snapshot: Value,
}

/// A storyboard before rendering: the document (pictures still empty) and the pictures to make.
#[derive(Clone, Debug)]
pub struct BoardPlan {
    pub title: String,
    /// `{fps, shots: [{index, shot, camera, start, end, seconds, describe, trajectory, frames:
    /// [{position, frame, describe, images: {}}]}]}`; hosts fill each frame's `images[pass]`.
    pub doc: Value,
    pub pictures: Vec<BoardShotPicture>,
    pub out: Option<String>,
}

/// A shot name usable in a file name.
pub fn file_safe(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(32).collect();
    if s.is_empty() { "shot".into() } else { s }
}

fn width_ok(w: Option<u32>, default: u32) -> CmdResult<u32> {
    let w = w.unwrap_or(default);
    if (16..=4096).contains(&w) { Ok(w) } else { Err(CommandError::invalid("width must be 16..=4096")) }
}

/// Plan a storyboard (no GPU).
pub fn storyboard_plan(scene: &Scene, params: Value) -> CmdResult<BoardPlan> {
    let p: StoryboardParams = parse(params)?;
    let width = width_ok(p.width, 960)?;
    let positions = p.positions.unwrap_or(vec![BoardPosition::First, BoardPosition::Middle, BoardPosition::Last]);
    let passes = p.passes.unwrap_or(vec![SnapshotPass::Clay]);
    if positions.is_empty() || passes.is_empty() {
        return Err(CommandError::invalid("positions and passes cannot be empty"));
    }
    let t = &scene.timeline;
    let all: Vec<(String, String, u32, u32)> = if t.shots.is_empty() {
        match &scene.active_camera {
            Some(c) => vec![("Shot".into(), c.clone(), t.start, t.end)],
            None => return Err(CommandError::new("failed", "the scene has no shots and no active camera")),
        }
    } else {
        t.shots.iter().map(|s| (s.name.clone(), s.camera.clone(), s.start, s.end)).collect()
    };
    let chosen: Vec<usize> = match p.shots {
        Some(ix) => {
            if let Some(bad) = ix.iter().find(|i| **i >= all.len()) {
                return Err(CommandError::invalid(format!("shot {bad} does not exist ({} shots)", all.len())));
            }
            ix
        }
        None => (0..all.len()).collect(),
    };
    if chosen.len() * positions.len() * passes.len() > 600 {
        return Err(CommandError::invalid("that storyboard has more than 600 pictures; pick fewer shots or passes"));
    }
    let describe = |cam: &str, f: u32| {
        let subject = auto_subject(scene, cam, f as f32);
        shotstage_core::eval::describe_shot(scene, cam, f as f32, subject.as_deref())
            .ok()
            .and_then(|d| serde_json::to_value(d).ok())
            .unwrap_or(Value::Null)
    };
    let mut shots = vec![];
    let mut pictures = vec![];
    for (n, &i) in chosen.iter().enumerate() {
        let (name, cam, a, b) = &all[i];
        scene.camera(cam).map_err(CommandError::from)?;
        let mut frames = vec![];
        for (k, pos) in positions.iter().enumerate() {
            let f = match pos {
                BoardPosition::First => *a,
                BoardPosition::Middle => (a + b) / 2,
                BoardPosition::Last => *b,
            };
            let label = match pos {
                BoardPosition::First => "first",
                BoardPosition::Middle => "middle",
                BoardPosition::Last => "last",
            };
            for pass in &passes {
                let pass_name = serde_json::to_value(pass).ok().and_then(|v| v.as_str().map(str::to_string));
                let pass_name = pass_name.unwrap_or_else(|| "clay".into());
                pictures.push(BoardShotPicture {
                    shot: n,
                    frame_index: k,
                    pass: *pass,
                    file: format!("{:02}_{}_{label}_{pass_name}.png", i + 1, file_safe(name)),
                    snapshot: json!({ "camera": cam, "frame": f, "pass": pass, "width": width }),
                });
            }
            frames.push(json!({ "position": pos, "frame": f, "describe": describe(cam, f), "images": {} }));
        }
        let traj = shotstage_core::eval::camera_trajectory(
            scene,
            cam,
            *a,
            *b,
            width,
            (width as f32 / shotstage_core::eval::camera_data_at(scene, cam, *a as f32)?.aspect).round() as u32,
        )
        .ok()
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(Value::Null);
        shots.push(json!({
            "index": i, "shot": name, "camera": cam, "start": a, "end": b,
            "seconds": (b - a + 1) as f32 / t.fps,
            "describe": describe(cam, (a + b) / 2),
            "frames": frames, "trajectory": traj,
        }));
    }
    Ok(BoardPlan {
        title: p.title.unwrap_or_else(|| "Storyboard".into()),
        doc: json!({ "fps": t.fps, "shots": shots }),
        pictures,
        out: p.out,
    })
}

impl BoardPlan {
    /// Record where picture `i` ended up (a file name or a data URL).
    pub fn set_image(&mut self, i: usize, src: &str) {
        let pic = &self.pictures[i];
        let pass = serde_json::to_value(pic.pass).unwrap_or(json!("clay"));
        let key = pass.as_str().unwrap_or("clay").to_string();
        self.doc["shots"][pic.shot]["frames"][pic.frame_index]["images"][key] = json!(src);
    }

    /// A self-contained page: every shot as a row of its frames, clay pictures with captions.
    pub fn html(&self) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        let mut body = String::new();
        for (n, shot) in self.doc["shots"].as_array().into_iter().flatten().enumerate() {
            body.push_str(&format!(
                "<section><h2><span>{}</span>{}</h2><p>{} · {} · frames {}–{} · {:.1}s</p><div class=row>",
                n + 1,
                esc(shot["shot"].as_str().unwrap_or_default()),
                esc(shot["camera"].as_str().unwrap_or_default()),
                esc(shot["describe"]["summary"].as_str().unwrap_or_default()),
                shot["start"],
                shot["end"],
                shot["seconds"].as_f64().unwrap_or(0.0),
            ));
            for f in shot["frames"].as_array().into_iter().flatten() {
                let images = f["images"].as_object().cloned().unwrap_or_default();
                let main = images.get("clay").or_else(|| images.values().next()).and_then(Value::as_str);
                let extra: Vec<String> = images
                    .iter()
                    .filter(|(k, _)| k.as_str() != "clay")
                    .map(|(k, v)| format!("<a href=\"{}\">{}</a>", esc(v.as_str().unwrap_or_default()), esc(k)))
                    .collect();
                body.push_str(&format!(
                    "<figure>{}<figcaption><b>{} · frame {}</b><span>{}</span>{}</figcaption></figure>",
                    main.map(|s| format!("<img src=\"{}\" alt=\"\">", esc(s))).unwrap_or_default(),
                    esc(f["position"].as_str().unwrap_or_default()),
                    f["frame"],
                    esc(f["describe"]["summary"].as_str().unwrap_or_default()),
                    if extra.is_empty() { String::new() } else { format!("<span>{}</span>", extra.join(" · ")) },
                ));
            }
            body.push_str("</div></section>\n");
        }
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<title>{t}</title><style>body{{font:14px system-ui,sans-serif;margin:24px;background:#fff;color:#111}}\
h1{{font-size:20px;margin:0 0 8px}}h2{{font-size:16px;margin:24px 0 2px}}h2 span{{color:#888;margin-right:8px}}\
section>p{{margin:0 0 8px;color:#666}}.row{{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:12px}}\
figure{{margin:0}}img{{width:100%;display:block;border:1px solid #ccc;border-radius:4px}}\
figcaption{{margin-top:4px;display:flex;flex-direction:column;gap:2px;font-size:12px}}figcaption span{{color:#666}}\
@media print{{body{{margin:0}}section{{break-inside:avoid}}}}</style></head><body><h1>{t}</h1>\n{body}</body></html>",
            t = esc(&self.title)
        )
    }
}

// ---------------------------------------------------------------- sequence

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SequenceParams {
    /// Passes. Default `["clay"]`.
    pub passes: Option<Vec<SnapshotPass>>,
    /// Picture width (16..=4096). Default 1280 (640 in the browser, where frames come back inline).
    pub width: Option<u32>,
    /// First frame. Default: the timeline start.
    pub start: Option<u32>,
    /// Last frame. Default: the timeline end.
    pub end: Option<u32>,
    /// Render every n-th frame. Default 1.
    pub step: Option<u32>,
    /// Look through this camera for every frame. Default: follow the shots (else the active
    /// camera).
    pub camera: Option<String>,
    /// Folder for `<pass>/00000.png`, camera.json and the videos (CLI / MCP; required there).
    pub out: Option<String>,
    /// Also make a video of each pass: `<pass>.mp4` in `out` with ffmpeg (CLI / MCP, when ffmpeg
    /// is installed); in the browser an MP4 per pass comes back as base64 in `videos` (WebCodecs),
    /// instead of the frames. Every frame then has the first camera's shape, in even pixels.
    pub video: Option<bool>,
}

/// One frame of a sequence and the snapshots to render for it.
#[derive(Clone, Debug, Serialize)]
pub struct SequenceFrame {
    pub frame: u32,
    pub camera: String,
    /// `render.snapshot` params per pass.
    pub snapshots: Vec<(SnapshotPass, Value)>,
}

#[derive(Clone, Debug)]
pub struct SequencePlan {
    pub fps: f32,
    pub width: u32,
    pub passes: Vec<SnapshotPass>,
    pub frames: Vec<SequenceFrame>,
    /// Per-frame camera data (the `frames` of camera.json).
    pub cameras: Vec<Value>,
    pub depth_range: DepthMap,
    pub out: Option<String>,
    pub video: bool,
}

pub fn pass_name(p: SnapshotPass) -> &'static str {
    match p {
        SnapshotPass::Clay => "clay",
        SnapshotPass::Depth => "depth",
        SnapshotPass::Normal => "normal",
    }
}

/// Plan a sequence (no GPU). `default_width` differs by host (inline frames are smaller).
pub fn sequence_plan(scene: &Scene, params: Value, default_width: u32) -> CmdResult<SequencePlan> {
    use shotstage_core::eval::{camera_data_at, camera_id_at, camera_trajectory, world_at};
    let p: SequenceParams = parse(params)?;
    let width = width_ok(p.width, default_width)?;
    let t = &scene.timeline;
    let (a, b) = (p.start.unwrap_or(t.start), p.end.unwrap_or(t.end));
    if b < a {
        return Err(CommandError::invalid("end must be >= start"));
    }
    let step = p.step.unwrap_or(1).max(1);
    let passes = p.passes.unwrap_or(vec![SnapshotPass::Clay]);
    if passes.is_empty() {
        return Err(CommandError::invalid("passes cannot be empty"));
    }
    if let Some(c) = &p.camera {
        scene.camera(c)?;
    }
    let cam_at = |f: u32| p.camera.clone().or_else(|| camera_id_at(scene, f as f32));
    // one depth map for the whole sequence (so the greys do not flicker): the auto fit of a
    // few frames along it, joined
    let depth_range = if passes.contains(&SnapshotPass::Depth) {
        let frames: Vec<u32> = (a..=b).step_by(step as usize).collect();
        let every = frames.len().div_ceil(24).max(1);
        frames
            .iter()
            .step_by(every)
            .chain(frames.last())
            .filter_map(|&f| {
                let cam = cam_at(f)?;
                let lens = camera_data_at(scene, &cam, f as f32).ok()?;
                let view = world_at(scene, &cam, f as f32).ok()?.inverse();
                Some(shotstage_core::depthfit::scene_fit(scene, f as f32, view, lens.projection(lens.aspect)))
            })
            .reduce(DepthMap::union)
            .unwrap_or_default()
    } else {
        DepthMap::default()
    };
    // a video needs one even size for every frame (H.264 is 4:2:0): the first camera's shape
    let video = p.video.unwrap_or(false);
    let (width, video_height) = if video {
        let cam = cam_at(a).ok_or_else(|| CommandError::new("failed", "no camera: add one (or a shot) first"))?;
        let aspect = camera_data_at(scene, &cam, a as f32)?.aspect;
        let w = (width & !1).max(16);
        (w, Some((((w as f32 / aspect) / 2.0).round() as u32 * 2).clamp(16, 4096)))
    } else {
        (width, None)
    };
    let mut frames = vec![];
    let mut cameras = vec![];
    for f in (a..=b).step_by(step as usize) {
        let cam = cam_at(f).ok_or_else(|| CommandError::new("failed", "no camera: add one (or a shot) first"))?;
        let snapshots = passes
            .iter()
            .map(|pass| {
                let mut s = json!({ "camera": cam, "frame": f, "pass": pass, "width": width });
                if let Some(h) = video_height {
                    s["height"] = json!(h);
                }
                if *pass == SnapshotPass::Depth {
                    s["depthRange"] = json!(depth_range.to_vec());
                }
                (*pass, s)
            })
            .collect();
        let lens = camera_data_at(scene, &cam, f as f32)?;
        let h = (width as f32 / lens.aspect).round() as u32;
        let one = camera_trajectory(scene, &cam, f, f, width, h)?;
        cameras.push(json!({ "frame": f, "camera": cam, "data": one.first() }));
        frames.push(SequenceFrame { frame: f, camera: cam, snapshots });
    }
    if frames.len() * passes.len() > 20_000 {
        return Err(CommandError::invalid("more than 20000 pictures; use a shorter range or `step`"));
    }
    Ok(SequencePlan { fps: t.fps, width, passes, frames, cameras, depth_range, out: p.out, video })
}

/// `camera.json` of a sequence.
pub fn sequence_cameras(plan: &SequencePlan) -> Value {
    json!({ "fps": plan.fps, "depthRange": plan.depth_range.to_vec(), "frames": plan.cameras })
}

// ---------------------------------------------------------------- finishing (all hosts)

fn data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", crate::model::encode(png))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_file(path: &std::path::Path, bytes: &[u8]) -> CmdResult<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| CommandError::new("failed", format!("creating {}: {e}", dir.display())))?;
    }
    std::fs::write(path, bytes).map_err(|e| CommandError::new("failed", format!("writing {}: {e}", path.display())))
}

/// Finish a rendered storyboard: with `out`, write the pictures, storyboard.json and
/// index.html there and return the document with file names; else return it with data URLs
/// and the page as `html`.
pub fn finish_storyboard(mut plan: BoardPlan, pngs: &[Vec<u8>]) -> CmdResult<Value> {
    match plan.out.clone() {
        #[cfg(not(target_arch = "wasm32"))]
        Some(dir) => {
            let dir = std::path::PathBuf::from(dir);
            for (i, png) in pngs.iter().enumerate() {
                let file = plan.pictures[i].file.clone();
                write_file(&dir.join(&file), png)?;
                plan.set_image(i, &file);
            }
            let json = serde_json::to_string_pretty(&plan.doc).unwrap_or_default();
            write_file(&dir.join("storyboard.json"), json.as_bytes())?;
            write_file(&dir.join("index.html"), plan.html().as_bytes())?;
            let mut doc = plan.doc.clone();
            doc["folder"] = json!(dir.display().to_string());
            doc["index"] = json!(dir.join("index.html").display().to_string());
            Ok(doc)
        }
        #[cfg(target_arch = "wasm32")]
        Some(_) => Err(needs_files(STORYBOARD_COMMAND, "out")),
        None => {
            for (i, png) in pngs.iter().enumerate() {
                plan.set_image(i, &data_url(png));
            }
            let mut doc = plan.doc.clone();
            doc["html"] = json!(plan.html());
            Ok(doc)
        }
    }
}

/// Collects a sequence as it renders: into `out` (files, as they come) or inline (data URLs).
pub struct SequenceSink {
    pub plan: SequencePlan,
    inline: Vec<Value>,
}

impl SequenceSink {
    pub fn new(plan: SequencePlan) -> CmdResult<Self> {
        if plan.out.is_none() && plan.frames.len() * plan.passes.len() > 1000 {
            return Err(CommandError::invalid(
                "more than 1000 pictures to return inline: give `out`, a shorter range or a larger `step`",
            ));
        }
        #[cfg(target_arch = "wasm32")]
        if plan.out.is_some() {
            return Err(needs_files(SEQUENCE_COMMAND, "out"));
        }
        Ok(Self { plan, inline: vec![] })
    }

    /// Every picture to render, in order: (frame index, pass, snapshot params).
    pub fn jobs(&self) -> Vec<(usize, SnapshotPass, Value)> {
        self.plan
            .frames
            .iter()
            .enumerate()
            .flat_map(|(i, f)| f.snapshots.iter().map(move |(p, v)| (i, *p, v.clone())))
            .collect()
    }

    pub fn add(&mut self, index: usize, pass: SnapshotPass, png: &[u8]) -> CmdResult<()> {
        match &self.plan.out {
            #[cfg(not(target_arch = "wasm32"))]
            Some(dir) => {
                write_file(&std::path::Path::new(dir).join(pass_name(pass)).join(format!("{index:05}.png")), png)
            }
            #[cfg(target_arch = "wasm32")]
            Some(_) => Err(needs_files(SEQUENCE_COMMAND, "out")),
            None => {
                if self.inline.len() <= index {
                    let f = &self.plan.frames[index];
                    self.inline.push(json!({ "frame": f.frame, "camera": f.camera, "images": {} }));
                }
                self.inline[index]["images"][pass_name(pass)] = json!(data_url(png));
                Ok(())
            }
        }
    }

    /// Write camera.json (and the videos, when asked for and ffmpeg is there); the result.
    pub fn finish(self) -> CmdResult<Value> {
        let plan = &self.plan;
        let passes: Vec<_> = plan.passes.iter().map(|p| pass_name(*p)).collect();
        let Some(dir) = plan.out.clone() else {
            return Ok(json!({
                "fps": plan.fps, "passes": passes, "depthRange": plan.depth_range.to_vec(),
                "frames": self.inline, "cameras": sequence_cameras(plan),
            }));
        };
        #[cfg(target_arch = "wasm32")]
        {
            let _ = dir;
            Err(needs_files(SEQUENCE_COMMAND, "out"))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let out = std::path::PathBuf::from(dir);
            let cams = serde_json::to_string_pretty(&sequence_cameras(plan)).unwrap_or_default();
            write_file(&out.join("camera.json"), cams.as_bytes())?;
            let mut videos = vec![];
            let mut warnings = vec![];
            if plan.video {
                let step = match plan.frames.as_slice() {
                    [a, b, ..] => (b.frame - a.frame).max(1),
                    _ => 1,
                };
                let fps = plan.fps / step as f32;
                for name in &passes {
                    let mp4 = out.join(format!("{name}.mp4"));
                    let status = std::process::Command::new("ffmpeg")
                        .args(["-y", "-loglevel", "error", "-framerate", &fps.to_string(), "-i"])
                        .arg(out.join(name).join("%05d.png"))
                        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"])
                        .arg(&mp4)
                        .status();
                    match status {
                        Ok(s) if s.success() => videos.push(mp4.display().to_string()),
                        Ok(_) => warnings.push(format!("ffmpeg could not encode {name}.mp4")),
                        Err(_) => {
                            warnings
                                .push("ffmpeg was not found: the PNG frames are there to encode yourself".to_string());
                            break;
                        }
                    }
                }
            }
            Ok(json!({
                "folder": out.display().to_string(), "frames": plan.frames.len(), "passes": passes,
                "fps": plan.fps, "depthRange": plan.depth_range.to_vec(), "videos": videos, "warnings": warnings,
            }))
        }
    }
}

/// Run a host command that renders, with a synchronous `render(snapshot params) -> PNG`.
/// Native hosts (CLI, desktop editor) use this; the browser runs the same steps asynchronously.
pub fn render_with(
    command: &str,
    scene: &Scene,
    params: Value,
    render: &mut dyn FnMut(Value) -> CmdResult<Vec<u8>>,
) -> Option<CmdResult<Value>> {
    let r = match command {
        STORYBOARD_COMMAND => storyboard_plan(scene, params).and_then(|plan| {
            let mut pngs = Vec::with_capacity(plan.pictures.len());
            for p in &plan.pictures {
                pngs.push(render(p.snapshot.clone())?);
            }
            finish_storyboard(plan, &pngs)
        }),
        crate::sheet::SHEET_COMMAND => crate::sheet::sheet_plan(scene, params).and_then(|plan| {
            let mut pngs = Vec::with_capacity(plan.pictures.len());
            for p in &plan.pictures {
                pngs.push(render(p.clone())?);
            }
            crate::sheet::finish_sheet(&plan, &pngs)
        }),
        SEQUENCE_COMMAND => sequence_plan(scene, params, 1280).and_then(SequenceSink::new).and_then(|mut sink| {
            let jobs = sink.jobs();
            for (i, pass, snap) in jobs {
                let png = render(snap)?;
                sink.add(i, pass, &png)?;
            }
            sink.finish()
        }),
        _ => return None,
    };
    Some(r)
}

// ---------------------------------------------------------------- files (native hosts)

/// Read the files named by `path` params into the inline fields the commands take, so a host
/// with a file system accepts `{"path": "chair.glb"}` wherever a command takes file contents.
/// Returns the params to dispatch and, for `model.export`, the file to write the result to.
#[cfg(not(target_arch = "wasm32"))]
pub fn resolve_paths(command: &str, mut params: Value) -> CmdResult<(Value, Option<std::path::PathBuf>)> {
    use std::path::PathBuf;
    let Some(path) = params.get("path").and_then(Value::as_str).map(PathBuf::from) else {
        return Ok((params, None));
    };
    let read = |p: &std::path::Path| {
        std::fs::read(p).map_err(|e| CommandError::new("failed", format!("reading {}: {e}", p.display())))
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let o = params.as_object_mut().ok_or_else(|| CommandError::invalid("params must be an object"))?;
    match command {
        "model.import" => {
            o.remove("path");
            let bytes = read(&path)?;
            if !o.contains_key("format") {
                let f = crate::model::ModelFormat::from_extension(&ext)
                    .ok_or_else(|| CommandError::invalid(format!("unknown model format `.{ext}`: give `format`")))?;
                o.insert("format".into(), json!(f.extension()));
            }
            if !o.contains_key("name")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                o.insert("name".into(), json!(stem));
            }
            if o.get("format").and_then(Value::as_str) == Some("obj")
                && !o.contains_key("mtl")
                && let Ok(text) = std::fs::read_to_string(path.with_extension("mtl"))
            {
                o.insert("mtl".into(), json!(text));
            }
            o.insert("data".into(), json!(crate::model::encode(&bytes)));
            Ok((params, None))
        }
        "image.import" | "environment.import" => {
            o.remove("path");
            let bytes = read(&path)?;
            if !o.contains_key("name")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                o.insert("name".into(), json!(stem));
            }
            o.insert("data".into(), json!(crate::model::encode(&bytes)));
            Ok((params, None))
        }
        "model.export" => {
            o.remove("path");
            if !o.contains_key("format") {
                let f = crate::model::ModelFormat::from_extension(&ext)
                    .ok_or_else(|| CommandError::invalid(format!("unknown model format `.{ext}`: give `format`")))?;
                o.insert("format".into(), json!(f.extension()));
            }
            if !o.contains_key("name")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                o.insert("name".into(), json!(stem));
            }
            Ok((params, Some(path)))
        }
        _ => Ok((params, None)),
    }
}

/// Write a `model.export` result to `path` (plus the `.mtl` of an OBJ) and slim the result.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_export(path: &std::path::Path, mut result: Value) -> CmdResult<Value> {
    use base64::Engine as _;
    let write = write_file;
    if let Some(d) = result.get("data").and_then(Value::as_str) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(d)
            .map_err(|e| CommandError::new("failed", e.to_string()))?;
        write(path, &bytes)?;
    } else if let Some(t) = result.get("text").and_then(Value::as_str) {
        write(path, t.as_bytes())?;
    }
    if let Some(m) = result.get("mtl").and_then(Value::as_str) {
        write(&path.with_extension("mtl"), m.as_bytes())?;
    }
    if let Some(o) = result.as_object_mut() {
        o.remove("data");
        o.remove("text");
        o.remove("mtl");
        o.insert("file".into(), json!(path.display().to_string()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn shot_scene() -> Session {
        let mut s = Session::default();
        s.batch(vec![crate::Call { command: "scene.template".into(), params: json!({"name": "packshot"}) }]).unwrap();
        s
    }

    #[test]
    fn catalog_lists_host_commands_once() {
        let s = Session::default();
        let list = s.list();
        for c in host_commands() {
            assert_eq!(list.iter().filter(|i| i.id == c.id).count(), 1, "{}", c.id);
            let e = Session::default().dispatch(c.id, json!({})).unwrap_err();
            assert_eq!(e.code, "unsupported", "{}", c.id);
        }
        let mut ids: Vec<_> = list.iter().map(|c| c.id.clone()).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate command ids");
    }

    #[test]
    fn storyboard_plan_covers_every_shot() {
        let s = shot_scene();
        let shots = s.scene().timeline.shots.len();
        assert!(shots >= 1);
        let mut plan = storyboard_plan(s.scene(), json!({"passes": ["clay", "depth"], "width": 320})).unwrap();
        assert_eq!(plan.pictures.len(), shots * 3 * 2);
        assert_eq!(plan.doc["shots"].as_array().unwrap().len(), shots);
        plan.set_image(0, "a.png");
        assert_eq!(plan.doc["shots"][0]["frames"][0]["images"]["clay"], "a.png");
        let html = plan.html();
        assert!(html.contains("<img src=\"a.png\""));
        assert!(storyboard_plan(s.scene(), json!({"shots": [99]})).is_err());
        assert!(storyboard_plan(Session::default().scene(), json!({})).is_err());
    }

    #[test]
    fn describe_finds_the_subject() {
        let mut s = Session::default();
        s.dispatch("scene.template", json!({"name": "interview"})).unwrap();
        // every shot of the interview has a person in it: the shot size is known
        for shot in s.scene().timeline.shots.clone() {
            let d = s.dispatch("camera.describe", json!({"id": shot.camera, "frame": shot.start})).unwrap().result;
            assert!(d["shotSize"].is_string(), "{}: {d}", shot.name);
            assert!(
                d["summary"].as_str().unwrap().contains("shot") || d["summary"].as_str().unwrap().contains("close-up")
            );
        }
        // looking at nothing: no subject
        s.dispatch(
            "entity.create",
            json!({"type": "camera", "id": "sky", "position": [0, 50, 0], "rotationEuler": [90, 0, 0]}),
        )
        .unwrap();
        assert!(auto_subject(s.scene(), "sky", 0.0).is_none());
    }

    #[test]
    fn sequence_plan_shares_one_depth_range() {
        let s = shot_scene();
        let p = sequence_plan(s.scene(), json!({"passes": ["depth"], "start": 0, "end": 9, "step": 3}), 640).unwrap();
        assert_eq!(p.frames.iter().map(|f| f.frame).collect::<Vec<_>>(), vec![0, 3, 6, 9]);
        let ranges: Vec<_> = p.frames.iter().map(|f| f.snapshots[0].1["depthRange"].clone()).collect();
        assert!(ranges.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(p.cameras.len(), 4);
        assert!(sequence_plan(s.scene(), json!({"start": 5, "end": 1}), 640).is_err());
    }

    #[test]
    fn view_commands() {
        let s = shot_scene();
        let mut v = ViewState::default();
        let id = s.scene().entities[0].id.clone();
        view_command(&mut v, s.scene(), "editor.select", json!({"id": id})).unwrap().unwrap();
        assert_eq!(v.selected.as_deref(), Some(id.as_str()));
        assert!(view_command(&mut v, s.scene(), "editor.select", json!({"id": "ghost"})).unwrap().is_err());
        view_command(&mut v, s.scene(), "editor.setFrame", json!({"frame": 12})).unwrap().unwrap();
        view_command(&mut v, s.scene(), "editor.setView", json!({"pass": "depth", "lookThrough": true}))
            .unwrap()
            .unwrap();
        let st = view_command(&mut v, s.scene(), "editor.state", json!({})).unwrap().unwrap();
        assert_eq!(st["frame"], 12.0);
        assert_eq!(st["pass"], "depth");
        assert!(view_command(&mut v, s.scene(), "entity.create", json!({})).is_none());
    }

    #[test]
    fn uv_editor_view() {
        let mut s = Session::default();
        s.dispatch(
            "entity.create",
            json!({"id": "box", "type": "mesh", "primitive": {"shape": "cube", "size": [1, 1, 1]}}),
        )
        .unwrap();
        let mut v = ViewState::default();
        assert!(view_command(&mut v, s.scene(), "editor.uvSelect", json!({"faces": [0]})).unwrap().is_err());
        let r =
            view_command(&mut v, s.scene(), "editor.uvEditor", json!({"id": "box", "mode": "face"})).unwrap().unwrap();
        assert_eq!(r["mode"], "face");
        assert_eq!(r["sticky"], true);
        let r = view_command(&mut v, s.scene(), "editor.uvSelect", json!({"faces": [1], "corners": [[0, 2]]}))
            .unwrap()
            .unwrap();
        assert_eq!(r["selection"].as_array().unwrap().len(), 5);
        assert!(view_command(&mut v, s.scene(), "editor.uvSelect", json!({"corners": [[0, 7]]})).unwrap().is_err());
        let st = view_command(&mut v, s.scene(), "editor.state", json!({})).unwrap().unwrap();
        assert_eq!(st["uv"]["id"], "box");
        view_command(&mut v, s.scene(), "editor.uvEditor", json!({"id": null})).unwrap().unwrap();
        assert!(v.uv.is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn paths_become_inline_data() {
        let dir = std::env::temp_dir().join(format!("shotstage-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let obj = dir.join("box.obj");
        std::fs::write(&obj, "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        let (p, w) = resolve_paths("model.import", json!({"path": obj.to_str().unwrap()})).unwrap();
        assert!(w.is_none());
        assert_eq!(p["format"], "obj");
        assert_eq!(p["name"], "box");
        let mut s = Session::default();
        s.dispatch("model.import", p).unwrap();
        let out = dir.join("out.obj");
        let (p, w) = resolve_paths("model.export", json!({"path": out.to_str().unwrap()})).unwrap();
        let r = s.dispatch("model.export", p).unwrap().result;
        let r = write_export(&w.unwrap(), r).unwrap();
        assert!(r.get("text").is_none());
        assert!(std::fs::read_to_string(&out).unwrap().contains("v "));
        // without a host the path is refused, not ignored
        let e = s.dispatch("model.import", json!({"path": "x.glb"})).unwrap_err();
        assert_eq!(e.code, "unsupported");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
