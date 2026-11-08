//! The shot sheet (分镜表): every shot with its picture, shot size, angle, focal length, camera
//! move, duration and notes, laid out as a table or a grid of panels, as a PDF or one PNG, to
//! hand to a crew.
//!
//! Analysis (`shot.list`) and layout run here without a GPU; hosts render one picture per shot
//! with `render.snapshot` (the plan's `pictures`) and hand the PNGs to [`finish_sheet`]. The
//! page is drawn into an image with the fonts compiled in (Latin + a Chinese subset, plus a
//! system Chinese font on desktops), so every host produces the same file.

use std::sync::OnceLock;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use glam::Vec3;
use image::{Rgb, RgbImage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shotstage_core::Scene;
use shotstage_core::eval::{camera_data_at, describe_shot, world_at};

use crate::{CmdResult, CommandError, parse};

pub const SHEET_COMMAND: &str = "render.shotSheet";

/// Chinese glyphs (Noto Sans SC, SIL OFL; the interface's characters plus the 3755 most common
/// ones). The editor uses the same bytes for its own text (a `static`, so there is one copy).
pub static CJK_FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-subset.otf");

/// Latin fonts a host already carries (the editor passes egui's), so the browser bundle holds
/// one copy; without one, native hosts use egui's default font from `epaint_default_fonts`.
static LATIN: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());

/// Use `bytes` (a TrueType / OpenType font) for Latin text on sheets. Call before the first
/// sheet is drawn.
pub fn register_latin_font(bytes: Vec<u8>) {
    if let Ok(mut v) = LATIN.lock()
        && !v.contains(&bytes)
    {
        v.push(bytes);
    }
}

// ---------------------------------------------------------------- params

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SheetFormat {
    /// A4 landscape pages.
    #[default]
    Pdf,
    /// One tall image with every shot.
    Png,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SheetLayout {
    /// One row per shot: number, picture, shot, size and angle, lens, move, duration, notes.
    #[default]
    Table,
    /// Panels in columns, a caption under each picture (storyboard style).
    Grid,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SheetLang {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh")]
    Zh,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShotSheetParams {
    /// "pdf" (A4 landscape pages, default) or "png" (one image).
    pub format: Option<SheetFormat>,
    /// "table" (one row per shot, default) or "grid" (storyboard panels).
    pub layout: Option<SheetLayout>,
    /// Panels per row in the grid layout (2..=5). Default 3.
    pub columns: Option<u32>,
    /// Language of the labels and film terms: "en" (default) or "zh".
    pub lang: Option<SheetLang>,
    /// Only these shots (0-based indices). Default every shot; with no shots, the whole
    /// timeline through the active camera.
    pub shots: Option<Vec<usize>>,
    /// Title at the top. Default "Shot sheet" / "分镜表".
    pub title: Option<String>,
    /// File to write (CLI / MCP; a folder gets shot-sheet.pdf / .png). Without it the file
    /// comes back as base64 `data`. MCP defaults to `<scene file>.shot-sheet.pdf`.
    pub out: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShotListParams {
    /// Only these shots (0-based indices). Default every shot.
    pub shots: Option<Vec<usize>>,
    /// Language of `movement`, `shotSizeText`...: "en" (default) or "zh".
    pub lang: Option<SheetLang>,
}

// ---------------------------------------------------------------- analysis

/// One part of a camera move: what (`pushIn`, `panLeft`, `zoomIn`, `arcRight`, `handheld`,
/// `static`...) and how much (metres, degrees or millimetres of focal length).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CameraMove {
    pub kind: &'static str,
    pub amount: f32,
}

/// A shot as a crew reads it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotRow {
    /// 1-based position in the edit.
    pub number: usize,
    pub id: String,
    pub name: String,
    pub camera: String,
    pub camera_name: String,
    pub start: u32,
    pub end: u32,
    pub frames: u32,
    pub seconds: f32,
    /// Film term at the middle frame ("medium shot"...), when something is in frame.
    pub shot_size: Option<String>,
    /// Shot size at the first and last frame, when it changes during the shot.
    pub shot_size_change: Option<[String; 2]>,
    pub angle: String,
    pub subject: Option<String>,
    /// Focal length at the first and last frame (mm).
    pub focal_length_mm: [f32; 2],
    pub lens: String,
    pub moves: Vec<CameraMove>,
    /// The move in words, e.g. "push in 1.2 m + pan left 30°".
    pub movement: String,
    pub notes: String,
    /// The same in the requested language (film terms translated).
    pub text: RowText,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowText {
    pub shot_size: String,
    pub angle: String,
    pub lens: String,
    pub movement: String,
    pub duration: String,
    pub frames: String,
}

/// How the camera moves between `a` and `b`, from its sampled path. With a `subject`, a camera
/// that keeps its distance from a moving subject is `tracking`.
pub fn camera_moves(scene: &Scene, cam: &str, a: u32, b: u32, subject: Option<&str>) -> CmdResult<Vec<CameraMove>> {
    let n = (b - a).min(240);
    let sample = |k: u32| -> CmdResult<(Vec3, Vec3, Vec3, f32)> {
        let f = if n == 0 { a as f32 } else { a as f32 + (b - a) as f32 * k as f32 / n as f32 };
        let w = world_at(scene, cam, f)?;
        let lens = camera_data_at(scene, cam, f)?;
        let fwd = w.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
        let right = w.transform_vector3(Vec3::X).normalize_or_zero();
        Ok((w.w_axis.truncate(), fwd, right, lens.focal_length_mm))
    };
    // yaw: + turns left (counter-clockwise seen from above); pitch: + looks up
    let yaw = |f: Vec3| (-f.x).atan2(-f.z).to_degrees();
    let pitch = |f: Vec3| f.y.clamp(-1.0, 1.0).asin().to_degrees();
    let wrap = |d: f32| (d + 540.0).rem_euclid(360.0) - 180.0;
    // per step, in the camera's own frame at the time (what an arc is made of), and how often
    // the motion turns back on itself (what a shaky handheld camera does)
    let (mut truck, mut pan, mut tilt) = (0.0f32, 0.0f32, 0.0f32);
    let (mut flips, mut last_turn, mut last_dp) = (0usize, [0.0f32; 2], Vec3::ZERO);
    let at = |k: u32| if n == 0 { a as f32 } else { a as f32 + (b - a) as f32 * k as f32 / n as f32 };
    let subject_at = |k: u32| subject.and_then(|s| world_at(scene, s, at(k)).ok()).map(|m| m.w_axis.truncate());
    let first = sample(0)?;
    let offset0 = subject_at(0).map(|s| first.0 - s);
    let (mut path, mut drift) = (0.0f32, 0.0f32);
    let mut prev = first;
    for k in 1..=n {
        let cur = sample(k)?;
        path += (cur.0 - prev.0).length();
        if let (Some(o), Some(s)) = (offset0, subject_at(k)) {
            drift = drift.max((cur.0 - s - o).length());
        }
        let dp = cur.0 - prev.0;
        truck += dp.dot(prev.2);
        // yaw is meaningless looking straight up or down
        let dyaw = if prev.1.y.abs() < 0.97 && cur.1.y.abs() < 0.97 { wrap(yaw(cur.1) - yaw(prev.1)) } else { 0.0 };
        let dpitch = pitch(cur.1) - pitch(prev.1);
        pan += dyaw;
        tilt += dpitch;
        for (i, d) in [dyaw, dpitch].into_iter().enumerate() {
            if d.abs() > 0.02 {
                flips += (d * last_turn[i] < 0.0) as usize;
                last_turn[i] = d;
            }
        }
        for i in 0..3 {
            if dp[i].abs() > 5e-4 {
                flips += (dp[i] * last_dp[i] < 0.0) as usize;
                last_dp[i] = dp[i];
            }
        }
        prev = cur;
    }
    let (f0, f1) = (first.3, prev.3);
    let mut out = vec![];
    let zoom = f1 / f0.max(1e-3);
    let zooming = !(0.95..=1.05).contains(&zoom);
    // following the subject at the same distance
    if let Some(o) = offset0
        && path > 0.5
        && drift < 0.15 * o.length() + 0.05
    {
        out.push(CameraMove { kind: "tracking", amount: (prev.0 - first.0).length() });
        if zooming {
            out.push(CameraMove { kind: if zoom > 1.0 { "zoomIn" } else { "zoomOut" }, amount: f1 - f0 });
        }
        return Ok(out);
    }
    if n >= 8 && flips >= (n as usize / 8).max(6) {
        out.push(CameraMove { kind: "handheld", amount: 0.0 });
    }
    // the overall move, in the first frame's terms: forward and sideways on the ground, up
    let d = prev.0 - first.0;
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
    let (fwd0, right0) = (flat(first.1), flat(first.2));
    let mut push = if fwd0 == Vec3::ZERO { -d.y * first.1.y.signum() } else { d.dot(fwd0) };
    let ped = if fwd0 == Vec3::ZERO { 0.0 } else { d.y };
    if right0 != Vec3::ZERO && pan.abs() <= 10.0 {
        truck = d.dot(right0);
    }
    const LIN: f32 = 0.05;
    const ANG: f32 = 3.0;
    // travelling sideways while turning the other way keeps something in frame: an arc
    if pan.abs() > 10.0 && truck.abs() > 0.1 && truck * pan > 0.0 {
        out.push(CameraMove { kind: if truck > 0.0 { "arcRight" } else { "arcLeft" }, amount: pan.abs() });
        (pan, truck) = (0.0, 0.0);
        // moving in or out during the arc: the change of distance to where the views cross
        let (a0, a1) = (glam::Vec2::new(first.1.x, first.1.z), glam::Vec2::new(prev.1.x, prev.1.z));
        let (p0, p1) = (glam::Vec2::new(first.0.x, first.0.z), glam::Vec2::new(prev.0.x, prev.0.z));
        let det = a0.perp_dot(a1);
        push = if det.abs() > 1e-3 {
            let t = (p1 - p0).perp_dot(a1) / det;
            let c = p0 + a0 * t;
            (p0 - c).length() - (p1 - c).length()
        } else {
            0.0
        };
    }
    if push.abs() > LIN && zooming && (push > 0.0) != (zoom > 1.0) {
        out.push(CameraMove { kind: "dollyZoom", amount: push.abs() });
    } else {
        if push.abs() > LIN {
            out.push(CameraMove { kind: if push > 0.0 { "pushIn" } else { "pullOut" }, amount: push.abs() });
        }
        if zooming {
            out.push(CameraMove { kind: if zoom > 1.0 { "zoomIn" } else { "zoomOut" }, amount: f1 - f0 });
        }
    }
    if truck.abs() > LIN {
        out.push(CameraMove { kind: if truck > 0.0 { "truckRight" } else { "truckLeft" }, amount: truck.abs() });
    }
    if ped.abs() > LIN {
        let kind = match (ped > 0.0, ped.abs() > 1.0) {
            (true, true) => "craneUp",
            (false, true) => "craneDown",
            (true, false) => "pedestalUp",
            (false, false) => "pedestalDown",
        };
        out.push(CameraMove { kind, amount: ped.abs() });
    }
    if pan.abs() > ANG {
        out.push(CameraMove { kind: if pan > 0.0 { "panLeft" } else { "panRight" }, amount: pan.abs() });
    }
    if tilt.abs() > ANG {
        out.push(CameraMove { kind: if tilt > 0.0 { "tiltUp" } else { "tiltDown" }, amount: tilt.abs() });
    }
    if out.is_empty() {
        out.push(CameraMove { kind: "static", amount: 0.0 });
    }
    Ok(out)
}

fn zh(lang: SheetLang) -> bool {
    lang == SheetLang::Zh
}

/// A film term in the sheet's language.
fn term(lang: SheetLang, en: &str) -> String {
    if !zh(lang) {
        return en.to_string();
    }
    match en {
        "extreme close-up" => "大特写",
        "close-up" => "特写",
        "medium close-up" => "近景",
        "medium shot" => "中景",
        "medium full shot" => "中全景",
        "full shot" => "全景",
        "wide shot" => "远景",
        "extreme wide shot" => "大远景",
        "high angle" => "俯拍",
        "eye level" => "平视",
        "low angle" => "仰拍",
        "bird's-eye" => "顶拍",
        "worm's-eye" => "大仰拍",
        "ultra-wide" => "超广角",
        "wide" => "广角",
        "normal" => "标准",
        "portrait" => "中长焦",
        "telephoto" => "长焦",
        other => other,
    }
    .to_string()
}

fn num(x: f32) -> String {
    let s = format!("{x:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// One move in words.
pub fn move_text(m: &CameraMove, f: [f32; 2], lang: SheetLang) -> String {
    let (m_en, m_zh) = (format!("{} m", num(m.amount)), format!("{} 米", num(m.amount)));
    let deg = format!("{:.0}°", m.amount);
    let mm = format!("{:.0}→{:.0} mm", f[0], f[1]);
    let (en, zh_word): (String, String) = match m.kind {
        "static" => ("static".into(), "固定镜头".into()),
        "handheld" => ("handheld".into(), "手持".into()),
        "tracking" => (format!("tracking {m_en}"), format!("跟拍 {m_zh}")),
        "arcLeft" => (format!("arc left {deg}"), format!("向左环绕 {deg}")),
        "arcRight" => (format!("arc right {deg}"), format!("向右环绕 {deg}")),
        "dollyZoom" => (format!("dolly zoom {mm}"), format!("滑动变焦 {mm}")),
        "pushIn" => (format!("push in {m_en}"), format!("推 {m_zh}")),
        "pullOut" => (format!("pull out {m_en}"), format!("拉 {m_zh}")),
        "zoomIn" => (format!("zoom in {mm}"), format!("变焦推 {mm}")),
        "zoomOut" => (format!("zoom out {mm}"), format!("变焦拉 {mm}")),
        "truckLeft" => (format!("truck left {m_en}"), format!("左移 {m_zh}")),
        "truckRight" => (format!("truck right {m_en}"), format!("右移 {m_zh}")),
        "pedestalUp" => (format!("pedestal up {m_en}"), format!("升 {m_zh}")),
        "pedestalDown" => (format!("pedestal down {m_en}"), format!("降 {m_zh}")),
        "craneUp" => (format!("crane up {m_en}"), format!("摇臂升 {m_zh}")),
        "craneDown" => (format!("crane down {m_en}"), format!("摇臂降 {m_zh}")),
        "panLeft" => (format!("pan left {deg}"), format!("左摇 {deg}")),
        "panRight" => (format!("pan right {deg}"), format!("右摇 {deg}")),
        "tiltUp" => (format!("tilt up {deg}"), format!("上摇 {deg}")),
        "tiltDown" => (format!("tilt down {deg}"), format!("下摇 {deg}")),
        other => (other.to_string(), other.to_string()),
    };
    if zh(lang) { zh_word } else { en }
}

/// The shots to list: (index, name, id, camera, start, end, notes).
type ShotSpec = (usize, String, String, String, u32, u32, String);

fn chosen_shots(scene: &Scene, shots: Option<Vec<usize>>) -> CmdResult<Vec<ShotSpec>> {
    let t = &scene.timeline;
    let all: Vec<ShotSpec> = if t.shots.is_empty() {
        match &scene.active_camera {
            Some(c) => vec![(0, "Shot".into(), String::new(), c.clone(), t.start, t.end, String::new())],
            None => return Err(CommandError::new("failed", "the scene has no shots and no active camera")),
        }
    } else {
        t.shots
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.name.clone(), s.id.clone(), s.camera.clone(), s.start, s.end, s.notes.clone()))
            .collect()
    };
    match shots {
        Some(ix) => {
            if let Some(bad) = ix.iter().find(|i| **i >= all.len()) {
                return Err(CommandError::invalid(format!("shot {bad} does not exist ({} shots)", all.len())));
            }
            Ok(ix.into_iter().map(|i| all[i].clone()).collect())
        }
        None => Ok(all),
    }
}

/// Every chosen shot analysed: framing, lens, move, timing.
pub fn shot_rows(scene: &Scene, shots: Option<Vec<usize>>, lang: SheetLang) -> CmdResult<Vec<ShotRow>> {
    let fps = scene.timeline.fps.max(1.0);
    let mut rows = vec![];
    for (i, name, id, cam, a, b, notes) in chosen_shots(scene, shots)? {
        scene.camera(&cam).map_err(CommandError::from)?;
        let mid = (a + b) / 2;
        let describe = |f: u32| {
            let subject = crate::host::auto_subject(scene, &cam, f as f32);
            describe_shot(scene, &cam, f as f32, subject.as_deref()).map_err(CommandError::from)
        };
        let d = describe(mid)?;
        let (d0, d1) = (describe(a)?, describe(b)?);
        let change = match (&d0.shot_size, &d1.shot_size) {
            (Some(x), Some(y)) if x != y => Some([x.clone(), y.clone()]),
            _ => None,
        };
        let focal = [d0.focal_length_mm, d1.focal_length_mm];
        let moves = camera_moves(scene, &cam, a, b, d.subject.as_deref())?;
        let words = |lang| moves.iter().map(|m| move_text(m, focal, lang)).collect::<Vec<_>>().join(" + ");
        let frames = b - a + 1;
        let seconds = frames as f32 / fps;
        let size_text = match &change {
            Some([x, y]) => format!("{} → {}", term(lang, x), term(lang, y)),
            None => d.shot_size.as_deref().map(|s| term(lang, s)).unwrap_or_else(|| "—".into()),
        };
        let lens_text = if (focal[1] - focal[0]).abs() >= 0.5 {
            format!("{:.0}→{:.0} mm", focal[0], focal[1])
        } else {
            format!("{:.0} mm", focal[0])
        };
        let text = RowText {
            shot_size: size_text,
            angle: term(lang, &d.angle),
            lens: format!("{lens_text} · {}", term(lang, &d.lens)),
            movement: words(lang),
            duration: if zh(lang) { format!("{} 秒", num(seconds)) } else { format!("{} s", num(seconds)) },
            frames: if zh(lang) { format!("{frames} 帧（{a}–{b}）") } else { format!("{frames} fr ({a}–{b})") },
        };
        rows.push(ShotRow {
            number: i + 1,
            id,
            name,
            camera_name: scene.get(&cam).map(|e| e.name.clone()).unwrap_or_else(|_| cam.clone()),
            camera: cam.clone(),
            start: a,
            end: b,
            frames,
            seconds,
            shot_size: d.shot_size.clone(),
            shot_size_change: change,
            angle: d.angle.clone(),
            subject: d.subject.clone(),
            focal_length_mm: focal,
            lens: d.lens.clone(),
            movement: words(SheetLang::En),
            moves,
            notes,
            text,
        });
    }
    Ok(rows)
}

/// `shot.list`: the rows of the shot sheet without pictures (no GPU).
pub fn shot_list(scene: &Scene, p: ShotListParams) -> CmdResult<Value> {
    let lang = p.lang.unwrap_or_default();
    let rows = shot_rows(scene, p.shots, lang)?;
    let total: f32 = rows.iter().map(|r| r.seconds).sum();
    Ok(json!({ "fps": scene.timeline.fps, "totalSeconds": total, "shots": rows }))
}

// ---------------------------------------------------------------- plan

/// Pixels per PDF point: pages are drawn at 150 dpi.
const S: f32 = 150.0 / 72.0;
/// A4 landscape, in points.
const PAGE: (f32, f32) = (842.0, 595.0);
const MARGIN: f32 = 24.0;
/// Table columns (points): number, picture, shot, size and angle, lens, move, duration; notes
/// take the rest.
const TABLE_COLS: [f32; 7] = [28.0, 142.0, 112.0, 96.0, 84.0, 130.0, 62.0];

/// A shot sheet before rendering: its rows and the pictures to make (one per row).
#[derive(Clone, Debug)]
pub struct SheetPlan {
    pub rows: Vec<ShotRow>,
    /// `render.snapshot` params, one per row (its middle frame).
    pub pictures: Vec<Value>,
    pub format: SheetFormat,
    pub layout: SheetLayout,
    pub columns: u32,
    pub lang: SheetLang,
    pub title: String,
    pub out: Option<String>,
    pub fps: f32,
}

/// Width of the picture box (points).
fn picture_width(layout: SheetLayout, columns: u32) -> f32 {
    match layout {
        SheetLayout::Table => TABLE_COLS[1] - 8.0,
        SheetLayout::Grid => (PAGE.0 - 2.0 * MARGIN - (columns - 1) as f32 * GRID_GAP) / columns as f32,
    }
}

const GRID_GAP: f32 = 14.0;
/// Caption line height in the grid layout (points).
const GRID_LINE: f32 = 10.5;

/// Plan a shot sheet (no GPU).
pub fn sheet_plan(scene: &Scene, params: Value) -> CmdResult<SheetPlan> {
    let p: ShotSheetParams = parse(params)?;
    let lang = p.lang.unwrap_or_default();
    let layout = p.layout.unwrap_or_default();
    let columns = p.columns.unwrap_or(3);
    if !(2..=5).contains(&columns) {
        return Err(CommandError::invalid("columns must be 2..=5"));
    }
    let rows = shot_rows(scene, p.shots, lang)?;
    if rows.len() > 300 {
        return Err(CommandError::invalid("more than 300 shots; pick some with `shots`"));
    }
    let format = p.format.unwrap_or_default();
    if format == SheetFormat::Png && rows.len() > 60 {
        return Err(CommandError::invalid("a PNG holds at most 60 shots; use format \"pdf\" or pick `shots`"));
    }
    let width = (picture_width(layout, columns) * S).round() as u32;
    let pictures = rows
        .iter()
        .map(|r| json!({ "camera": r.camera, "frame": (r.start + r.end) / 2, "pass": "clay", "width": width }))
        .collect();
    let title = p.title.unwrap_or_else(|| if zh(lang) { "分镜表" } else { "Shot sheet" }.into());
    Ok(SheetPlan { rows, pictures, format, layout, columns, lang, title, out: p.out, fps: scene.timeline.fps })
}

// ---------------------------------------------------------------- drawing

fn fonts() -> &'static [FontArc] {
    static FONTS: OnceLock<Vec<FontArc>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let latin: Vec<Vec<u8>> = LATIN.lock().map(|v| v.clone()).unwrap_or_default();
        let mut v: Vec<FontArc> =
            latin.into_iter().filter_map(|b| ab_glyph::FontVec::try_from_vec(b).ok().map(FontArc::new)).collect();
        #[cfg(not(target_arch = "wasm32"))]
        if v.is_empty()
            && let Ok(f) = FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT)
        {
            v.push(f);
        }
        if let Ok(f) = FontArc::try_from_slice(CJK_FONT) {
            v.push(f);
        }
        // a full system Chinese font covers rare characters the subset leaves out
        #[cfg(not(target_arch = "wasm32"))]
        for (path, index) in [
            ("/System/Library/Fonts/PingFang.ttc", 0),
            ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
            ("/System/Library/Fonts/STHeiti Light.ttc", 0),
            ("C:\\Windows\\Fonts\\msyh.ttc", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
            ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ] {
            if let Ok(bytes) = std::fs::read(path)
                && let Ok(f) = ab_glyph::FontVec::try_from_vec_and_index(bytes, index)
            {
                v.push(FontArc::new(f));
                break;
            }
        }
        v
    })
}

type Color = [u8; 3];
const INK: Color = [24, 24, 28];
const GREY: Color = [110, 110, 118];
const LINE: Color = [210, 210, 214];
const BAND: Color = [238, 239, 242];
const ACCENT: Color = [214, 120, 40];

/// A page being drawn, addressed in points.
struct Canvas {
    img: RgbImage,
}

impl Canvas {
    fn new(w: f32, h: f32) -> Self {
        let img = RgbImage::from_pixel((w * S).round() as u32, (h * S).round() as u32, Rgb([255, 255, 255]));
        Self { img }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Color) {
        let (x0, y0) = ((x * S).round().max(0.0) as u32, (y * S).round().max(0.0) as u32);
        let x1 = (((x + w) * S).round() as u32).min(self.img.width());
        let y1 = (((y + h) * S).round() as u32).min(self.img.height());
        for py in y0..y1 {
            for px in x0..x1 {
                self.img.put_pixel(px, py, Rgb(c));
            }
        }
    }

    fn hline(&mut self, x: f32, y: f32, w: f32, c: Color) {
        self.rect(x, y, w, 0.6, c);
    }

    fn vline(&mut self, x: f32, y: f32, h: f32, c: Color) {
        self.rect(x, y, 0.6, h, c);
    }

    fn frame(&mut self, x: f32, y: f32, w: f32, h: f32, c: Color) {
        self.hline(x, y, w, c);
        self.hline(x, y + h - 0.6, w, c);
        self.vline(x, y, h, c);
        self.vline(x + w - 0.6, y, h, c);
    }

    /// A picture fitted (letterboxed) into a box.
    fn picture(&mut self, png: &[u8], x: f32, y: f32, w: f32, h: f32) {
        self.rect(x, y, w, h, [60, 60, 66]);
        if let Ok(pic) = image::load_from_memory(png) {
            let pic = pic.to_rgb8();
            let (bw, bh) = (w * S, h * S);
            let k = (bw / pic.width() as f32).min(bh / pic.height() as f32);
            let (nw, nh) = ((pic.width() as f32 * k).round() as u32, (pic.height() as f32 * k).round() as u32);
            let pic = if (nw, nh) == pic.dimensions() {
                pic
            } else {
                image::imageops::resize(&pic, nw.max(1), nh.max(1), image::imageops::FilterType::Triangle)
            };
            let ox = (x * S + (bw - nw as f32) / 2.0).round() as i64;
            let oy = (y * S + (bh - nh as f32) / 2.0).round() as i64;
            image::imageops::replace(&mut self.img, &pic, ox, oy);
        }
        self.frame(x, y, w, h, [150, 150, 156]);
    }

    /// Draw `s` with its baseline at `y`; returns its width (points).
    fn text(&mut self, s: &str, x: f32, y: f32, size: f32, c: Color, bold: bool) -> f32 {
        layout_text(s, size, x, y, |font, glyph| {
            let Some(o) = font.outline_glyph(glyph) else { return };
            let b = o.px_bounds();
            let img = &mut self.img;
            let passes: &[i64] = if bold { &[0, 1] } else { &[0] };
            for dx in passes {
                o.draw(|gx, gy, cov| {
                    let px = b.min.x as i64 + gx as i64 + *dx;
                    let py = b.min.y as i64 + gy as i64;
                    if px < 0 || py < 0 || px >= img.width() as i64 || py >= img.height() as i64 {
                        return;
                    }
                    let p = img.get_pixel_mut(px as u32, py as u32);
                    let a = cov.clamp(0.0, 1.0);
                    for (v, ink) in p.0.iter_mut().zip(c) {
                        *v = (*v as f32 * (1.0 - a) + ink as f32 * a).round() as u8;
                    }
                });
            }
        })
    }
}

/// Lay out `s` at `size` points from `x`, baseline `y`, calling `draw` per glyph (pixels);
/// returns the width in points.
fn layout_text(s: &str, size: f32, x: f32, y: f32, mut draw: impl FnMut(&FontArc, ab_glyph::Glyph)) -> f32 {
    let fonts = fonts();
    let mut pen = x * S;
    for c in s.chars() {
        let font = fonts.iter().find(|f| f.glyph_id(c).0 != 0).or(fonts.first());
        let Some(font) = font else { break };
        let scale = PxScale::from(size * S * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0));
        let id = font.glyph_id(c);
        draw(font, id.with_scale_and_position(scale, point(pen, y * S)));
        pen += font.as_scaled(scale).h_advance(id);
    }
    pen / S - x
}

fn measure(s: &str, size: f32) -> f32 {
    layout_text(s, size, 0.0, 0.0, |_, _| {})
}

/// `s` cut to `w` points with an ellipsis.
fn fit(s: &str, size: f32, w: f32) -> String {
    if measure(s, size) <= w {
        return s.to_string();
    }
    let mut out: String = String::new();
    for c in s.chars() {
        out.push(c);
        if measure(&format!("{out}…"), size) > w {
            out.pop();
            break;
        }
    }
    format!("{}…", out.trim_end())
}

/// `s` wrapped to `w` points, at most `max` lines (the last one ellipsized).
fn wrap(s: &str, size: f32, w: f32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for para in s.lines() {
        let mut line = String::new();
        for c in para.chars() {
            let mut next = line.clone();
            next.push(c);
            // closing punctuation stays with the text before it
            let closing = "，。、；：？！”’）》】…,.;:?!)".contains(c);
            if measure(&next, size) > w && !line.is_empty() && !closing {
                // break Latin text at its last space
                let (keep, carry) = match line.rfind(' ') {
                    Some(i) if c != ' ' && i > 0 && line[i + 1..].chars().all(|x| x.is_ascii_alphanumeric()) => {
                        (line[..i].to_string(), line[i + 1..].to_string())
                    }
                    _ => (line.clone(), String::new()),
                };
                lines.push(keep);
                line = carry;
                if c != ' ' || !line.is_empty() {
                    line.push(c);
                }
            } else {
                line = next;
            }
        }
        lines.push(line);
    }
    if lines.len() > max {
        let rest = lines[max - 1..].join(" ");
        lines.truncate(max - 1);
        lines.push(fit(&rest, size, w));
        if let Some(l) = lines.last_mut()
            && !l.ends_with('…')
        {
            *l = fit(&format!("{l}……"), size, w);
        }
    }
    lines
}

struct Labels {
    no: &'static str,
    picture: &'static str,
    shot: &'static str,
    size: &'static str,
    lens: &'static str,
    movement: &'static str,
    duration: &'static str,
    notes: &'static str,
    page: &'static str,
}

fn labels(lang: SheetLang) -> Labels {
    if zh(lang) {
        Labels {
            no: "镜号",
            picture: "画面",
            shot: "镜头 / 机位",
            size: "景别 / 角度",
            lens: "焦距",
            movement: "运镜",
            duration: "时长",
            notes: "备注",
            page: "第 {} / {} 页",
        }
    } else {
        Labels {
            no: "#",
            picture: "Frame",
            shot: "Shot / camera",
            size: "Size / angle",
            lens: "Lens",
            movement: "Camera move",
            duration: "Duration",
            notes: "Notes",
            page: "Page {} of {}",
        }
    }
}

fn summary_line(plan: &SheetPlan) -> String {
    let total: f32 = plan.rows.iter().map(|r| r.seconds).sum();
    if zh(plan.lang) {
        format!("共 {} 个镜头 · 总时长 {} 秒 · {} fps", plan.rows.len(), num(total), num(plan.fps))
    } else {
        format!("{} shots · {} s in total · {} fps", plan.rows.len(), num(total), num(plan.fps))
    }
}

/// Item heights and what fits on a page.
struct Geometry {
    item_h: f32,
    per_row: usize,
    top: f32,
}

const HEADER_H: f32 = 64.0;
const FOOTER_H: f32 = 22.0;
const TABLE_HEAD: f32 = 18.0;

fn geometry(plan: &SheetPlan) -> Geometry {
    match plan.layout {
        SheetLayout::Table => {
            let pic_h = picture_width(plan.layout, plan.columns) * 9.0 / 16.0;
            Geometry { item_h: pic_h + 8.0, per_row: 1, top: HEADER_H + TABLE_HEAD }
        }
        SheetLayout::Grid => {
            let w = picture_width(plan.layout, plan.columns);
            let notes = if plan.rows.iter().any(|r| !r.notes.is_empty()) { 2.0 } else { 0.0 };
            Geometry {
                item_h: w * 9.0 / 16.0 + 13.0 + (4.0 + notes) * GRID_LINE + 6.0,
                per_row: plan.columns as usize,
                top: HEADER_H,
            }
        }
    }
}

fn draw_header(c: &mut Canvas, plan: &SheetPlan, width: f32) {
    let x = MARGIN;
    c.rect(x, MARGIN - 2.0, 4.0, 20.0, ACCENT);
    c.text(&fit(&plan.title, 16.0, width - 2.0 * MARGIN - 12.0), x + 10.0, MARGIN + 14.0, 16.0, INK, true);
    c.text(&summary_line(plan), x + 10.0, MARGIN + 29.0, 8.5, GREY, false);
    c.hline(x, HEADER_H - 5.0, width - 2.0 * MARGIN, LINE);
}

fn draw_footer(c: &mut Canvas, plan: &SheetPlan, page: usize, pages: usize, width: f32, height: f32) {
    let y = height - MARGIN + 6.0;
    c.text("shotstage", MARGIN, y, 7.0, GREY, false);
    if pages > 1 {
        let s = labels(plan.lang).page.replacen("{}", &(page + 1).to_string(), 1).replacen("{}", &pages.to_string(), 1);
        let w = measure(&s, 7.0);
        c.text(&s, width - MARGIN - w, y, 7.0, GREY, false);
    }
}

fn table_head(c: &mut Canvas, lang: SheetLang, y: f32, width: f32) {
    let l = labels(lang);
    c.rect(MARGIN, y, width - 2.0 * MARGIN, TABLE_HEAD, BAND);
    let names = [l.no, l.picture, l.shot, l.size, l.lens, l.movement, l.duration, l.notes];
    let mut x = MARGIN;
    for (i, n) in names.iter().enumerate() {
        c.text(n, x + 4.0, y + 12.5, 7.5, INK, true);
        x += TABLE_COLS.get(i).copied().unwrap_or(0.0);
    }
}

/// Text lines in a table cell, top-aligned.
fn cell(c: &mut Canvas, lines: &[(String, f32, Color, bool)], x: f32, y: f32) {
    let mut by = y + 12.0;
    for (s, size, col, bold) in lines {
        c.text(s, x, by, *size, *col, *bold);
        by += size * 1.45;
    }
}

fn table_row(c: &mut Canvas, plan: &SheetPlan, r: &ShotRow, png: &[u8], y: f32, width: f32, h: f32) {
    let mut x = MARGIN;
    let pad = 4.0;
    let w = |i: usize| TABLE_COLS[i] - 2.0 * pad;
    let notes_w = width - 2.0 * MARGIN - TABLE_COLS.iter().sum::<f32>();
    // number
    c.text(&r.number.to_string(), x + pad, y + 15.0, 11.0, ACCENT, true);
    x += TABLE_COLS[0];
    let pw = picture_width(plan.layout, plan.columns);
    c.picture(png, x + pad, y + 4.0, pw, pw * 9.0 / 16.0);
    x += TABLE_COLS[1];
    let t = &r.text;
    let mut shot = vec![(fit(&r.name, 9.0, w(2)), 9.0, INK, true)];
    shot.push((fit(&r.camera_name, 7.5, w(2)), 7.5, GREY, false));
    shot.push((fit(&t.frames, 7.5, w(2)), 7.5, GREY, false));
    cell(c, &shot, x + pad, y);
    x += TABLE_COLS[2];
    let mut size: Vec<_> = wrap(&t.shot_size, 8.5, w(3), 2).into_iter().map(|s| (s, 8.5, INK, false)).collect();
    size.push((fit(&t.angle, 7.5, w(3)), 7.5, GREY, false));
    cell(c, &size, x + pad, y);
    x += TABLE_COLS[3];
    let lens: Vec<_> = wrap(&t.lens, 8.5, w(4), 3).into_iter().map(|s| (s, 8.5, INK, false)).collect();
    cell(c, &lens, x + pad, y);
    x += TABLE_COLS[4];
    let max = ((h - 6.0) / (8.5 * 1.45)).floor().max(1.0) as usize;
    let mv: Vec<_> =
        wrap(&t.movement.replace(" + ", "\n"), 8.5, w(5), max).into_iter().map(|s| (s, 8.5, INK, false)).collect();
    cell(c, &mv, x + pad, y);
    x += TABLE_COLS[5];
    cell(c, &[(t.duration.clone(), 9.0, INK, true)], x + pad, y);
    x += TABLE_COLS[6];
    let notes: Vec<_> = wrap(&r.notes, 8.0, notes_w - 2.0 * pad, ((h - 6.0) / (8.0 * 1.45)).floor().max(1.0) as usize)
        .into_iter()
        .map(|s| (s, 8.0, INK, false))
        .collect();
    cell(c, &notes, x + pad, y);
    c.hline(MARGIN, y + h, width - 2.0 * MARGIN, LINE);
}

fn grid_panel(c: &mut Canvas, plan: &SheetPlan, r: &ShotRow, png: &[u8], x: f32, y: f32) {
    let w = picture_width(plan.layout, plan.columns);
    let ph = w * 9.0 / 16.0;
    c.picture(png, x, y, w, ph);
    // the number on the picture
    let n = r.number.to_string();
    let nw = measure(&n, 9.0) + 8.0;
    c.rect(x, y, nw, 14.0, ACCENT);
    c.text(&n, x + 4.0, y + 10.5, 9.0, [255, 255, 255], true);
    let t = &r.text;
    let mut by = y + ph + 12.0;
    let dur = format!("{} · {}", t.duration, t.frames);
    let dw = measure(&dur, 7.5);
    c.text(&dur, x + w - dw, by, 7.5, GREY, false);
    c.text(&fit(&r.name, 9.5, w - dw - 8.0), x, by, 9.5, INK, true);
    let l = labels(plan.lang);
    let sep = if zh(plan.lang) { "：" } else { ": " };
    let line = |k: &str, v: &str| format!("{k}{sep}{v}");
    for s in [
        line(l.size, &format!("{} · {}", t.shot_size, t.angle)),
        line(l.lens, &t.lens),
        line(l.movement, &t.movement),
        line(if zh(plan.lang) { "机位" } else { "Camera" }, &r.camera_name),
    ] {
        by += GRID_LINE;
        c.text(&fit(&s, 7.5, w), x, by, 7.5, INK, false);
    }
    if !r.notes.is_empty() {
        for s in wrap(&line(l.notes, &r.notes), 7.5, w, 2) {
            by += GRID_LINE;
            c.text(&s, x, by, 7.5, GREY, false);
        }
    }
}

/// The finished pages (one for a PNG).
pub struct Sheet {
    pub pages: Vec<RgbImage>,
}

/// Draw the sheet from the plan and one PNG per row.
pub fn compose(plan: &SheetPlan, pngs: &[Vec<u8>]) -> Sheet {
    let g = geometry(plan);
    let (width, page_h) = PAGE;
    let gap = match plan.layout {
        SheetLayout::Table => 0.0,
        SheetLayout::Grid => 12.0,
    };
    let body = page_h - g.top - FOOTER_H - MARGIN + gap;
    let rows_per_page = ((body / (g.item_h + gap)).floor() as usize).max(1);
    let grid_rows = plan.rows.len().div_ceil(g.per_row).max(1);
    let (pages, rows_per_page, height) = match plan.format {
        SheetFormat::Pdf => (grid_rows.div_ceil(rows_per_page), rows_per_page, page_h),
        SheetFormat::Png => (1, grid_rows, g.top + grid_rows as f32 * (g.item_h + gap) - gap + FOOTER_H + MARGIN + 6.0),
    };
    let empty = vec![];
    let mut out = vec![];
    for page in 0..pages {
        let mut c = Canvas::new(width, height);
        draw_header(&mut c, plan, width);
        if plan.layout == SheetLayout::Table {
            table_head(&mut c, plan.lang, HEADER_H, width);
        }
        for gr in 0..rows_per_page {
            let row = page * rows_per_page + gr;
            let y = g.top + gr as f32 * (g.item_h + gap);
            for col in 0..g.per_row {
                let i = row * g.per_row + col;
                let Some(r) = plan.rows.get(i) else { break };
                let png = pngs.get(i).unwrap_or(&empty);
                match plan.layout {
                    SheetLayout::Table => table_row(&mut c, plan, r, png, y, width, g.item_h),
                    SheetLayout::Grid => {
                        let x = MARGIN + col as f32 * (picture_width(plan.layout, plan.columns) + GRID_GAP);
                        grid_panel(&mut c, plan, r, png, x, y)
                    }
                }
            }
        }
        draw_footer(&mut c, plan, page, pages, width, height);
        out.push(c.img);
    }
    Sheet { pages: out }
}

fn encode_png(img: &RgbImage) -> CmdResult<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| CommandError::new("failed", e.to_string()))?;
    Ok(buf.into_inner())
}

/// The zlib stream of a PNG (its IDAT chunks), which PDF reads directly with the PNG predictors.
fn png_idat(png: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 8;
    while i + 8 <= png.len() {
        let len = u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
        let kind = &png[i + 4..i + 8];
        if kind == b"IDAT" && i + 8 + len <= png.len() {
            out.extend_from_slice(&png[i + 8..i + 8 + len]);
        }
        i += 12 + len;
    }
    out
}

fn pdf_text(s: &str) -> String {
    let mut hex = String::from("<FEFF");
    for u in s.encode_utf16() {
        hex.push_str(&format!("{u:04X}"));
    }
    hex.push('>');
    hex
}

/// A PDF with one full-page picture per page (lossless: the PNG's own compressed data).
pub fn pdf(sheet: &Sheet, title: &str) -> CmdResult<Vec<u8>> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = vec![];
    let n = sheet.pages.len();
    // objects: 1 catalog, 2 pages, 3 info, then per page: page, contents, image
    let page_id = |i: usize| 4 + i * 3;
    let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", page_id(i))).collect();
    obj(&mut out, &mut offsets, format!("<< /Type /Pages /Count {n} /Kids [{}] >>", kids.join(" ")).as_bytes());
    obj(&mut out, &mut offsets, format!("<< /Title {} /Producer (shotstage) >>", pdf_text(title)).as_bytes());
    for (i, img) in sheet.pages.iter().enumerate() {
        let (pw, ph) = (img.width() as f32 / S, img.height() as f32 / S);
        let id = page_id(i);
        obj(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Contents {} 0 R /Resources << /XObject << /P {} 0 R >> >> >>",
                id + 1,
                id + 2
            )
            .as_bytes(),
        );
        let content = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /P Do Q");
        obj(
            &mut out,
            &mut offsets,
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).as_bytes(),
        );
        let data = png_idat(&encode_png(img)?);
        let mut body = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 \
/Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns {w} >> /Length {} >>\nstream\n",
            data.len(),
            w = img.width(),
            h = img.height()
        )
        .into_bytes();
        body.extend_from_slice(&data);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offsets, &body);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1)
            .as_bytes(),
    );
    Ok(out)
}

impl SheetPlan {
    pub fn file_name(&self) -> &'static str {
        match self.format {
            SheetFormat::Pdf => "shot-sheet.pdf",
            SheetFormat::Png => "shot-sheet.png",
        }
    }

    pub fn mime(&self) -> &'static str {
        match self.format {
            SheetFormat::Pdf => "application/pdf",
            SheetFormat::Png => "image/png",
        }
    }

    /// The file's bytes.
    pub fn encode(&self, sheet: &Sheet) -> CmdResult<Vec<u8>> {
        match self.format {
            SheetFormat::Pdf => pdf(sheet, &self.title),
            SheetFormat::Png => encode_png(&sheet.pages[0]),
        }
    }
}

/// A smaller PNG of page `i` (for showing to a model or in a preview).
pub fn preview_png(sheet: &Sheet, i: usize, width: u32) -> CmdResult<Vec<u8>> {
    let page = &sheet.pages[i.min(sheet.pages.len().saturating_sub(1))];
    let h = (page.height() as f32 * width as f32 / page.width() as f32).round() as u32;
    encode_png(&image::imageops::resize(page, width, h.max(1), image::imageops::FilterType::Triangle))
}

/// Finish a rendered shot sheet: with `out` write the file there and return its path, else
/// return it as base64 `data`. Both carry the analysed `shots`.
pub fn finish_sheet(plan: &SheetPlan, pngs: &[Vec<u8>]) -> CmdResult<Value> {
    let sheet = compose(plan, pngs);
    let bytes = plan.encode(&sheet)?;
    let mut v = json!({
        "format": plan.format, "layout": plan.layout, "mimeType": plan.mime(), "fileName": plan.file_name(),
        "pages": sheet.pages.len(), "bytes": bytes.len(), "shots": plan.rows,
    });
    match &plan.out {
        #[cfg(not(target_arch = "wasm32"))]
        Some(out) => {
            let mut path = std::path::PathBuf::from(out);
            if path.is_dir() || out.ends_with('/') || out.ends_with('\\') {
                path = path.join(plan.file_name());
            }
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)
                    .map_err(|e| CommandError::new("failed", format!("creating {}: {e}", dir.display())))?;
            }
            std::fs::write(&path, &bytes)
                .map_err(|e| CommandError::new("failed", format!("writing {}: {e}", path.display())))?;
            v["path"] = json!(path.display().to_string());
        }
        #[cfg(target_arch = "wasm32")]
        Some(_) => return Err(crate::host::needs_files(SHEET_COMMAND, "out")),
        None => v["data"] = json!(crate::model::encode(&bytes)),
    }
    Ok(v)
}

pub fn commands() -> Vec<crate::Command> {
    vec![crate::command!(
        "shot.list",
        "Every shot as a crew reads it: number, name, camera, frames and seconds, shot size (and how it changes), angle, focal length, lens, the camera move worked out from the animation (push in, pan, arc, zoom, handheld, static...) and the shot's notes. The rows of render.shotSheet, without pictures.",
        mutates: false,
        ShotListParams,
        |s, p| shot_list(s, p)
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn scene() -> Session {
        let mut s = Session::default();
        s.dispatch("scene.template", json!({"name": "packshot"})).unwrap();
        s
    }

    #[test]
    fn moves_are_named() {
        let mut s = Session::default();
        s.dispatch("entity.create", json!({"type": "camera", "id": "c", "position": [0, 1.6, 5]})).unwrap();
        let kinds = |s: &Session| -> Vec<&'static str> {
            camera_moves(s.scene(), "c", 0, 48, None).unwrap().into_iter().map(|m| m.kind).collect()
        };
        assert_eq!(kinds(&s), ["static"]);
        let key = |s: &mut Session, f: u32, pos: [f32; 3], rot: [f32; 3]| {
            s.dispatch("entity.setTransform", json!({"id": "c", "position": pos, "rotationEuler": rot})).unwrap();
            s.dispatch("keyframe.set", json!({"id": "c", "property": "transform", "frame": f})).unwrap();
        };
        key(&mut s, 0, [0.0, 1.6, 5.0], [0.0, 0.0, 0.0]);
        key(&mut s, 48, [0.0, 1.6, 3.0], [0.0, 0.0, 0.0]);
        assert_eq!(kinds(&s), ["pushIn"]);
        key(&mut s, 48, [0.0, 1.6, 5.0], [0.0, 30.0, 0.0]);
        let m = camera_moves(s.scene(), "c", 0, 48, None).unwrap();
        assert_eq!(m[0].kind, "panLeft");
        assert!((m[0].amount - 30.0).abs() < 1.0, "{m:?}");
        assert_eq!(move_text(&m[0], [35.0, 35.0], SheetLang::Zh), "左摇 30°");
        // a quarter orbit around the origin, looking at it
        for k in 0..=4 {
            let a = (k as f32 * 22.5f32).to_radians();
            key(&mut s, k * 12, [5.0 * a.sin(), 1.6, 5.0 * a.cos()], [0.0, (k as f32) * 22.5, 0.0]);
        }
        assert_eq!(kinds(&s), ["arcRight"]);
    }

    #[test]
    fn shot_list_reads_like_a_shot_sheet() {
        let mut s = scene();
        let id = s.scene().timeline.shots[0].id.clone();
        s.dispatch("shot.set", json!({"id": id, "notes": "Hand enters, picks up the bottle"})).unwrap();
        let r = s.dispatch("shot.list", json!({"lang": "zh"})).unwrap().result;
        let first = &r["shots"][0];
        assert_eq!(first["number"], 1);
        assert_eq!(first["notes"], "Hand enters, picks up the bottle");
        assert!(first["text"]["lens"].as_str().unwrap().contains("mm"), "{first}");
        assert!(!first["movement"].as_str().unwrap().is_empty());
        assert!(r["totalSeconds"].as_f64().unwrap() > 0.0);
        assert!(s.dispatch("shot.list", json!({"shots": [99]})).is_err());
    }

    #[test]
    fn sheets_draw_and_encode() {
        let s = scene();
        for (format, layout, lang) in
            [("pdf", "table", "zh"), ("png", "grid", "en"), ("pdf", "grid", "zh"), ("png", "table", "en")]
        {
            let plan = sheet_plan(s.scene(), json!({"format": format, "layout": layout, "lang": lang})).unwrap();
            assert_eq!(plan.pictures.len(), plan.rows.len());
            // a flat grey picture stands in for the render
            let pic =
                RgbImage::from_pixel(plan.pictures[0]["width"].as_u64().unwrap() as u32, 90, Rgb([128, 128, 128]));
            let png = encode_png(&pic).unwrap();
            let pngs = vec![png; plan.rows.len()];
            let v = finish_sheet(&plan, &pngs).unwrap();
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap();
            if format == "pdf" {
                assert!(bytes.starts_with(b"%PDF-1.4") && bytes.ends_with(b"%%EOF\n"));
            } else {
                assert!(image::load_from_memory(&bytes).is_ok());
            }
        }
        assert!(sheet_plan(s.scene(), json!({"columns": 9})).is_err());
    }

    #[test]
    fn text_wraps_and_fits() {
        let lines = wrap("push in 1.2 m and then a very long note that keeps going", 8.0, 80.0, 3);
        assert!(lines.len() <= 3 && lines.iter().all(|l| measure(l, 8.0) <= 80.5), "{lines:?}");
        assert!(measure("分镜表", 10.0) > 20.0, "Chinese glyphs come from the Noto subset");
        assert!(fit("a long long long title", 10.0, 30.0).ends_with('…'));
    }
}
