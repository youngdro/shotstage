//! Evaluate the scene at a frame: animated transforms, world matrices, the camera in use and
//! a film-language description of the shot.

use glam::{Mat4, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::camera::{CameraData, Intrinsics};
use crate::scene::{Scene, Transform};
use crate::timeline::{KeyValue, TrackProperty};
use crate::{CoreError, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedEntity {
    pub id: String,
    /// Animated local transform.
    pub local: Transform,
    /// World matrix, column-major 4x4.
    pub world: [f32; 16],
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedScene {
    pub frame: f32,
    pub entities: Vec<EvaluatedEntity>,
    /// The camera this frame is seen through (shot camera, else the active camera).
    pub camera: Option<String>,
}

/// Animated local transform of `id` at `frame`.
pub fn local_at(scene: &Scene, id: &str, frame: f32) -> Result<Transform> {
    let e = scene.get(id)?;
    Ok(match scene.timeline.track(id, TrackProperty::Transform).and_then(|t| t.sample(frame)) {
        Some(KeyValue::Transform(t)) => t,
        _ => e.transform,
    })
}

pub fn world_at(scene: &Scene, id: &str, frame: f32) -> Result<Mat4> {
    let mut m = Mat4::IDENTITY;
    let mut cur = Some(id.to_string());
    let mut guard = 0;
    while let Some(c) = cur {
        m = local_at(scene, &c, frame)?.matrix() * m;
        cur = scene.get(&c)?.parent.clone();
        guard += 1;
        if guard > 10_000 {
            return Err(CoreError::Invalid("hierarchy too deep or cyclic".into()));
        }
    }
    Ok(m)
}

/// Camera lens at `frame` (focal length may be animated).
pub fn camera_data_at(scene: &Scene, id: &str, frame: f32) -> Result<CameraData> {
    let mut c = *scene.camera(id)?;
    if let Some(KeyValue::Scalar(f)) =
        scene.timeline.track(id, TrackProperty::FocalLength).and_then(|t| t.sample(frame))
    {
        c.focal_length_mm = f.clamp(1.0, 2000.0);
    }
    Ok(c)
}

/// Which camera `frame` is seen through.
pub fn camera_id_at(scene: &Scene, frame: f32) -> Option<String> {
    scene.timeline.shot_at(frame).map(|s| s.camera.clone()).or_else(|| scene.active_camera.clone())
}

pub fn evaluate(scene: &Scene, frame: f32) -> Result<EvaluatedScene> {
    let entities = scene
        .entities
        .iter()
        .map(|e| {
            Ok(EvaluatedEntity {
                id: e.id.clone(),
                local: local_at(scene, &e.id, frame)?,
                world: world_at(scene, &e.id, frame)?.to_cols_array(),
                visible: e.visible,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(EvaluatedScene { frame, entities, camera: camera_id_at(scene, frame) })
}

/// One frame of a camera trajectory, in the convention used by camera-conditioned video
/// models (RealEstate10K style): OpenCV camera axes (+X right, +Y down, +Z forward).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CameraFrame {
    pub frame: u32,
    pub time_seconds: f32,
    pub intrinsics: Intrinsics,
    pub focal_length_mm: f32,
    /// World-to-camera 3x4 matrix, row-major, OpenCV axes.
    pub w2c: [[f32; 4]; 3],
    /// Camera position in world space (scene axes, +Y up).
    pub position: [f32; 3],
}

pub fn camera_trajectory(
    scene: &Scene,
    camera: &str,
    start: u32,
    end: u32,
    width: u32,
    height: u32,
) -> Result<Vec<CameraFrame>> {
    if end < start || end - start > 100_000 {
        return Err(CoreError::Invalid("bad frame range".into()));
    }
    if !(16..=8192).contains(&width) || !(16..=8192).contains(&height) {
        return Err(CoreError::Invalid("width and height must be 16..=8192".into()));
    }
    // GL-style camera (-Z forward, +Y up) to OpenCV (+Z forward, +Y down).
    let gl_to_cv = Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0));
    (start..=end)
        .map(|f| {
            let fr = f as f32;
            let lens = camera_data_at(scene, camera, fr)?;
            let world = world_at(scene, camera, fr)?;
            let w2c = gl_to_cv * world.inverse();
            let r = w2c.transpose().to_cols_array_2d(); // rows of w2c
            Ok(CameraFrame {
                frame: f,
                time_seconds: (f - scene.timeline.start) as f32 / scene.timeline.fps,
                intrinsics: lens.intrinsics(width, height),
                focal_length_mm: lens.focal_length_mm,
                w2c: [r[0], r[1], r[2]],
                position: world.w_axis.truncate().to_array(),
            })
        })
        .collect()
}

/// A shot described in film vocabulary, so agents and prompts can talk about framing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShotDescription {
    pub camera: String,
    pub frame: f32,
    pub focal_length_mm: f32,
    /// "ultra-wide" | "wide" | "normal" | "portrait" | "telephoto"
    pub lens: String,
    /// Camera pitch in degrees (+ looks up).
    pub pitch_degrees: f32,
    /// "high angle" | "eye level" | "low angle" | "bird's-eye" | "worm's-eye"
    pub angle: String,
    pub subject: Option<String>,
    pub subject_distance_m: Option<f32>,
    /// Share of frame height the subject's bounds fill.
    pub subject_frame_fraction: Option<f32>,
    /// "extreme close-up" … "extreme wide shot"
    pub shot_size: Option<String>,
    /// One line for prompts, e.g. "medium shot, low angle, 35mm normal lens".
    pub summary: String,
}

pub fn describe_shot(scene: &Scene, camera: &str, frame: f32, subject: Option<&str>) -> Result<ShotDescription> {
    let lens = camera_data_at(scene, camera, frame)?;
    let world = world_at(scene, camera, frame)?;
    let fwd = world.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
    let pitch = fwd.y.clamp(-1.0, 1.0).asin().to_degrees();
    let angle = match pitch {
        p if p < -60.0 => "bird's-eye",
        p if p < -10.0 => "high angle",
        p if p <= 10.0 => "eye level",
        p if p <= 60.0 => "low angle",
        _ => "worm's-eye",
    };
    let f = lens.focal_length_mm * 36.0 / lens.sensor_width_mm; // full-frame equivalent
    let lens_word = match f {
        f if f < 20.0 => "ultra-wide",
        f if f < 35.0 => "wide",
        f if f < 70.0 => "normal",
        f if f < 135.0 => "portrait",
        _ => "telephoto",
    };
    let (mut dist, mut frac, mut size) = (None, None, None);
    if let Some(s) = subject {
        // how much of the frame the subject (with its children: a whole prefab group) covers
        let b = crate::spatial::bounds(scene, s, frame)?;
        let origin = world_at(scene, s, frame)?.w_axis.truncate();
        let b = b.unwrap_or(crate::spatial::Aabb {
            min: origin - Vec3::new(0.25, 0.85, 0.25),
            max: origin + Vec3::new(0.25, 0.85, 0.25),
        });
        let target = b.center();
        let eye = world.w_axis.truncate();
        let rot = world.to_scale_rotation_translation().1;
        let fr = crate::spatial::frame_fraction(&b, eye, rot, lens.vfov(), lens.aspect);
        dist = Some((target - eye).length());
        frac = Some(fr);
        size = Some(
            match fr {
                // fraction = whole subject height / frame height (a person framed waist-up ≈ 2)
                f if f > 7.0 => "extreme close-up",
                f if f > 3.5 => "close-up",
                f if f > 2.4 => "medium close-up",
                f if f > 1.6 => "medium shot",
                f if f > 1.15 => "medium full shot",
                f if f > 0.6 => "full shot",
                f if f > 0.25 => "wide shot",
                _ => "extreme wide shot",
            }
            .to_string(),
        );
    }
    let summary = format!(
        "{}{}, {:.0}mm {} lens",
        size.as_ref().map(|s| format!("{s}, ")).unwrap_or_default(),
        angle,
        lens.focal_length_mm,
        lens_word
    );
    Ok(ShotDescription {
        camera: camera.into(),
        frame,
        focal_length_mm: lens.focal_length_mm,
        lens: lens_word.into(),
        pitch_degrees: pitch,
        angle: angle.into(),
        subject: subject.map(str::to_string),
        subject_distance_m: dist,
        subject_frame_fraction: frac,
        shot_size: size,
        summary,
    })
}

/// An entity's mesh at `frame` in its own space, with modifiers applied. Skinned meshes are
/// bent by their joints' animated transforms; everything else is the plain evaluated mesh.
pub fn posed_mesh(scene: &Scene, id: &str, frame: f32) -> Result<crate::Mesh> {
    let (source, modifiers) = match &scene.get(id)?.kind {
        crate::EntityKind::Mesh { source, modifiers, .. } => (source, modifiers),
        crate::EntityKind::Image(img) => return Ok(img.card()),
        _ => return Err(CoreError::WrongKind(id.into(), "mesh")),
    };
    let crate::MeshSource::Skinned { mesh, skin } = source else {
        return crate::mesh::evaluate_mesh(source, modifiers);
    };
    skin.validate(mesh.positions.len())?;
    let to_local = world_at(scene, id, frame)?.inverse();
    // A joint that was deleted stays in its bind pose.
    let mats: Vec<Mat4> = skin
        .joints
        .iter()
        .zip(&skin.inverse_bind)
        .map(|(j, ib)| {
            let ib = Mat4::from_cols_array(ib);
            match world_at(scene, j, frame) {
                Ok(w) => to_local * w * ib,
                Err(_) => to_local * ib.inverse() * ib,
            }
        })
        .collect();
    let mut posed = mesh.clone();
    for (i, p) in posed.positions.iter_mut().enumerate() {
        let (js, ws) = (skin.vertex_joints[i], skin.vertex_weights[i]);
        let total: f32 = ws.iter().sum();
        if total <= 1e-6 {
            continue;
        }
        let v = Vec3::from_array(*p);
        let mut out = Vec3::ZERO;
        for k in 0..4 {
            if ws[k] > 0.0 {
                out += mats[js[k] as usize].transform_point3(v) * (ws[k] / total);
            }
        }
        *p = out.to_array();
    }
    let mut m = posed;
    for md in modifiers {
        m = md.apply(&m)?;
    }
    Ok(m)
}

/// An entity's mesh at `frame` triangulated (auto-smoothed normals) in its own space; cached by
/// content for everything but skinned meshes.
pub fn posed_tris(scene: &Scene, id: &str, frame: f32) -> Result<std::rc::Rc<crate::TriMesh>> {
    match &scene.get(id)?.kind {
        crate::EntityKind::Mesh { source, modifiers, .. } if !matches!(source, crate::MeshSource::Skinned { .. }) => {
            crate::meshcache::local_tris(source, modifiers)
        }
        _ => {
            Ok(std::rc::Rc::new(posed_mesh(scene, id, frame)?.triangulate_auto_smooth(crate::meshcache::SMOOTH_ANGLE)))
        }
    }
}

/// Visible lights at `frame`, in world space.
pub fn lights_at(scene: &Scene, frame: f32) -> Vec<crate::light::WorldLight> {
    scene
        .entities
        .iter()
        .filter_map(|e| {
            let crate::EntityKind::Light(l) = &e.kind else { return None };
            if crate::spatial::is_hidden(scene, &e.id) || l.intensity <= 0.0 {
                return None;
            }
            let w = world_at(scene, &e.id, frame).ok()?;
            let dir = w.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
            let half = (l.cone_degrees.to_radians() / 2.0).clamp(0.01, 1.56);
            let (inner, outer) = match l.kind {
                crate::LightKind::Spot => (half * (1.0 - l.softness.clamp(0.0, 1.0)), half),
                // a panel spreads its light over a wide, soft hemisphere
                crate::LightKind::Area => (0.35, 1.5),
                _ => (std::f32::consts::PI, std::f32::consts::PI),
            };
            Some(crate::light::WorldLight {
                kind: l.kind,
                position: w.w_axis.truncate().to_array(),
                direction: (if dir == Vec3::ZERO { Vec3::NEG_Y } else { dir }).to_array(),
                radiance: l.color.map(|c| c * l.intensity),
                cos_inner: inner.cos(),
                cos_outer: outer.cos(),
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::mesh::{MeshSource, Primitive};
    use crate::scene::{Entity, EntityKind};
    use crate::timeline::{Interp, Key};

    fn scene() -> Scene {
        let mut s = Scene::default();
        s.insert(Entity {
            id: "actor".into(),
            name: "Actor".into(),
            parent: None,
            transform: Transform::from_parts(Vec3::new(0.0, 0.85, 0.0), glam::Quat::IDENTITY, Vec3::ONE),
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: Primitive::Cube { size: [0.5, 1.7, 0.3] } },
                modifiers: vec![],
                color: [1.0; 3],
                material: None,
            },
        })
        .unwrap();
        let mut t = Transform::default();
        t.translation = [0.0, 0.85, 4.0];
        s.insert(Entity {
            id: "cam".into(),
            name: "Cam".into(),
            parent: None,
            transform: t,
            visible: true,
            locked: false,
            kind: EntityKind::Camera(CameraData::default()),
        })
        .unwrap();
        s.active_camera = Some("cam".into());
        s
    }

    #[test]
    fn animated_world_and_parenting() {
        let mut s = scene();
        let mut a = Transform::default();
        a.translation = [0.0, 0.0, 4.0];
        let mut b = a;
        b.translation = [2.0, 0.0, 4.0];
        let tr = s.timeline.track_mut_or_insert("cam", TrackProperty::Transform);
        tr.set_key(Key { frame: 0.0, value: KeyValue::Transform(a), interp: Interp::Linear });
        tr.set_key(Key { frame: 10.0, value: KeyValue::Transform(b), interp: Interp::Linear });
        let w = world_at(&s, "cam", 5.0).unwrap();
        assert!((w.w_axis.x - 1.0).abs() < 1e-5);
        s.set_parent("actor", Some("cam")).unwrap();
        assert!(s.set_parent("cam", Some("actor")).is_err());
        let ev = evaluate(&s, 10.0).unwrap();
        assert_eq!(ev.camera.as_deref(), Some("cam"));
    }

    #[test]
    fn describe_and_trajectory() {
        let s = scene();
        let d = describe_shot(&s, "cam", 0.0, Some("actor")).unwrap();
        assert_eq!(d.angle, "eye level");
        assert_eq!(d.lens, "normal");
        assert_eq!(d.shot_size.as_deref(), Some("full shot"));
        let traj = camera_trajectory(&s, "cam", 0, 2, 1280, 720).unwrap();
        assert_eq!(traj.len(), 3);
        // OpenCV: a point in front of the camera has positive z
        let r = traj[0].w2c;
        let p = [0.0f32, 0.85, 0.0, 1.0];
        let z: f32 = (0..4).map(|i| r[2][i] * p[i]).sum();
        assert!((z - 4.0).abs() < 1e-4);
    }
}
