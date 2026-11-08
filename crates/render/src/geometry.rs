//! CPU side: turn an evaluated scene into vertex lists. No GPU here, so it is unit-tested.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use shotstage_core::eval::{camera_data_at, posed_tris, world_at};
use shotstage_core::human::HumanPart;
use shotstage_core::spatial::{self, RayOptions, is_hidden};
use shotstage_core::{EntityKind, MeshSource, Scene};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
    /// Entity index + 1 (0 = none), for id passes and picking; the top four bits carry the
    /// surface look (0 matte, 1 skin, 2 hair, 3 eyes).
    pub id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
}

/// What to draw besides the meshes.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub selected: Option<String>,
    pub grid: bool,
    pub cameras: bool,
    /// Camera being looked through: its frustum is not drawn.
    pub looking_through: Option<String>,
    /// Entity under the pointer (lightly highlighted with its children).
    pub hovered: Option<String>,
    /// Entity being dragged: its footprint and a drop line to the ground are drawn.
    pub footprint: Option<String>,
    /// Ignore the scene's lights and use the flat studio clay light.
    pub studio_light: bool,
    /// Viewer position, so billboards can turn towards it (set by the renderer).
    pub eye: Option<[f32; 3]>,
    /// More selected entities (multi-selection), highlighted like `selected`.
    pub also_selected: Vec<String>,
    /// Turn shadows off (they are on by default).
    pub no_shadows: bool,
}

/// Vertex of a textured image card.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ImageVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Multiplies the picture (selection / hover highlight).
    pub tint: [f32; 3],
}

/// One image card to draw with its asset's texture.
pub struct ImageQuad {
    pub asset: String,
    /// Entity index + 1.
    pub id: u32,
    pub vertices: [ImageVertex; 6],
}

pub use shotstage_core::image::card_world as image_world;

/// Vertex of a textured mesh: shaded like [`Vertex`], coloured by its texture.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct TexVertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Multiplies the texture (mesh colour, selection highlight).
    pub color: [f32; 3],
}

/// The triangles of one textured mesh.
pub struct TexBatch {
    pub asset: String,
    /// Entity index + 1.
    pub id: u32,
    pub vertices: Vec<TexVertex>,
}

#[derive(Default)]
pub struct Geometry {
    pub triangles: Vec<Vertex>,
    pub lines: Vec<LineVertex>,
    pub images: Vec<ImageQuad>,
    pub textured: Vec<TexBatch>,
}

impl Geometry {
    /// Every shaded triangle corner (plain and textured), for bounds and shadows.
    pub fn solid_positions(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.triangles.iter().map(|v| v.pos).chain(self.textured.iter().flat_map(|b| b.vertices.iter().map(|v| v.pos)))
    }

    /// Every opaque triangle (plain, textured and pictures) with its entity index + 1.
    pub fn id_triangles(&self) -> impl Iterator<Item = (u32, [Vec3; 3])> + '_ {
        let p = |a: [f32; 3]| Vec3::from_array(a);
        let plain = self
            .triangles
            .as_chunks::<3>()
            .0
            .iter()
            .map(move |t| (t[0].id & 0x0fff_ffff, [p(t[0].pos), p(t[1].pos), p(t[2].pos)]));
        let tex = self.textured.iter().flat_map(move |b| {
            b.vertices.as_chunks::<3>().0.iter().map(move |t| (b.id, [p(t[0].pos), p(t[1].pos), p(t[2].pos)]))
        });
        let img = self.images.iter().flat_map(move |q| {
            q.vertices.as_chunks::<3>().0.iter().map(move |t| (q.id, [p(t[0].pos), p(t[1].pos), p(t[2].pos)]))
        });
        plain.chain(tex).chain(img)
    }
}

const SELECT: [f32; 3] = [1.0, 0.62, 0.18];
const CAMERA: [f32; 3] = [0.35, 0.6, 1.0];

pub fn build(scene: &Scene, frame: f32, overlay: &Overlay) -> Geometry {
    let mut g = Geometry::default();
    if overlay.grid {
        grid(&mut g.lines);
    }
    let group = |id: &Option<String>| id.as_deref().map(|i| spatial::subtree(scene, i)).unwrap_or_default();
    let (mut selected_set, hovered_set) = (group(&overlay.selected), group(&overlay.hovered));
    for a in &overlay.also_selected {
        selected_set.extend(spatial::subtree(scene, a));
    }
    // joints that skinned meshes follow are not drawn as loose empties
    let skin_joints: std::collections::HashSet<&str> = scene
        .entities
        .iter()
        .flat_map(|e| match &e.kind {
            EntityKind::Mesh { source: shotstage_core::MeshSource::Skinned { skin, .. }, .. } => skin.joints.as_slice(),
            EntityKind::Mesh { source: shotstage_core::MeshSource::Human { joints, .. }, .. } => joints.as_slice(),
            _ => &[],
        })
        .map(String::as_str)
        .collect();
    for (index, e) in scene.entities.iter().enumerate() {
        if is_hidden(scene, &e.id) {
            continue;
        }
        let Ok(world) = world_at(scene, &e.id, frame) else { continue };
        let selected = selected_set.contains(&e.id);
        let hovered = !selected && hovered_set.contains(&e.id);
        match &e.kind {
            EntityKind::Mesh { color, material, source, .. } => {
                let Ok(tri) = posed_tris(scene, &e.id, frame) else { continue };
                let textured = material
                    .as_ref()
                    .and_then(|m| m.texture.as_ref().filter(|k| scene.assets.contains_key(*k)).map(|k| (m, k)));
                // surface look for the shader (top four bits of the id)
                let look: u32 = match source {
                    MeshSource::Human { part: HumanPart::Skin, .. } => 1,
                    MeshSource::Human { part: HumanPart::Hair, .. } => 2,
                    MeshSource::Human { part: HumanPart::Eyes | HumanPart::Irises, .. } => 3,
                    _ => 0,
                };
                let normal_m = world.inverse().transpose();
                let c = if selected {
                    mix(*color, SELECT, 0.45)
                } else if hovered {
                    mix(*color, [1.0; 3], 0.25)
                } else {
                    *color
                };
                if let Some((mat, key)) = textured {
                    let uvs = shotstage_core::uv::tri_uvs(&tri, mat);
                    let vertices = tri
                        .indices
                        .iter()
                        .zip(uvs)
                        .map(|(&i, uv)| {
                            let i = i as usize;
                            TexVertex {
                                pos: world.transform_point3(Vec3::from_array(tri.positions[i])).to_array(),
                                normal: normal_m
                                    .transform_vector3(Vec3::from_array(tri.normals[i]))
                                    .normalize_or_zero()
                                    .to_array(),
                                uv,
                                color: c,
                            }
                        })
                        .collect();
                    g.textured.push(TexBatch { asset: key.clone(), id: index as u32 + 1, vertices });
                    continue;
                }
                for &i in &tri.indices {
                    let i = i as usize;
                    g.triangles.push(Vertex {
                        pos: world.transform_point3(Vec3::from_array(tri.positions[i])).to_array(),
                        normal: normal_m
                            .transform_vector3(Vec3::from_array(tri.normals[i]))
                            .normalize_or_zero()
                            .to_array(),
                        color: c,
                        id: (index as u32 + 1) | look << 28,
                    });
                }
            }
            EntityKind::Image(img) => {
                let w = image_world(world, img.billboard, overlay.eye.map(Vec3::from_array));
                let (hw, hh) = (img.size[0] / 2.0, img.size[1] / 2.0);
                let tint = if selected {
                    [1.0, 0.82, 0.6]
                } else if hovered {
                    [1.12, 1.12, 1.12]
                } else {
                    [1.0; 3]
                };
                let n = w.transform_vector3(Vec3::Z).normalize_or_zero().to_array();
                let v = |x: f32, y: f32, u: f32, vv: f32| ImageVertex {
                    pos: w.transform_point3(Vec3::new(x, y, 0.0)).to_array(),
                    normal: n,
                    uv: [u, vv],
                    tint,
                };
                let (a, b, c, d) =
                    (v(-hw, -hh, 0.0, 1.0), v(hw, -hh, 1.0, 1.0), v(hw, hh, 1.0, 0.0), v(-hw, hh, 0.0, 0.0));
                g.images.push(ImageQuad {
                    asset: img.asset.clone(),
                    id: index as u32 + 1,
                    vertices: [a, b, c, a, c, d],
                });
                if selected && overlay.cameras {
                    for (p, q) in [(a.pos, b.pos), (b.pos, c.pos), (c.pos, d.pos), (d.pos, a.pos)] {
                        line(&mut g.lines, Vec3::from_array(p), Vec3::from_array(q), SELECT);
                    }
                }
            }
            EntityKind::Light(l) if overlay.cameras => {
                let warm = [l.color[0] * 0.6 + 0.4, l.color[1] * 0.6 + 0.35, l.color[2] * 0.4 + 0.15];
                light_icon(&mut g.lines, world, l, if selected { SELECT } else { warm });
            }
            EntityKind::Camera(_) if overlay.cameras && overlay.looking_through.as_deref() != Some(e.id.as_str()) => {
                if let Ok(lens) = camera_data_at(scene, &e.id, frame) {
                    frustum(&mut g.lines, world, lens.hfov(), lens.vfov(), if selected { SELECT } else { CAMERA });
                }
            }
            // group roots and a person's joints stay invisible unless selected
            EntityKind::Empty
                if overlay.cameras
                    && (overlay.selected.as_deref() == Some(e.id.as_str())
                        || (scene.children(Some(&e.id)).next().is_none() && !skin_joints.contains(e.id.as_str()))) =>
            {
                let c = if selected { SELECT } else { [0.8, 0.8, 0.8] };
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    line(&mut g.lines, world.transform_point3(-axis * 0.25), world.transform_point3(axis * 0.25), c);
                }
            }
            _ => {}
        }
    }
    if let Some(id) = &overlay.footprint
        && let Ok(Some(b)) = spatial::bounds(scene, id, frame)
    {
        // outline of the footprint where it rests, and a plumb line when it floats
        let y = b.min.y + 0.005;
        let c = [b.min, Vec3::new(b.max.x, y, b.min.z), Vec3::new(b.max.x, y, b.max.z), Vec3::new(b.min.x, y, b.max.z)];
        let c = c.map(|p| Vec3::new(p.x, y, p.z));
        for i in 0..4 {
            line(&mut g.lines, c[i], c[(i + 1) % 4], SELECT);
        }
        let mid = Vec3::new(b.center().x, y, b.center().z);
        if y > 0.02 {
            line(&mut g.lines, mid, Vec3::new(mid.x, 0.0, mid.z), [0.6, 0.6, 0.6]);
        }
        let s = 0.08;
        line(&mut g.lines, mid - Vec3::X * s, mid + Vec3::X * s, SELECT);
        line(&mut g.lines, mid - Vec3::Z * s, mid + Vec3::Z * s, SELECT);
    }
    g
}

/// Wire icon of a light: a small star at the light, plus its beam (sun arrow, spot cone, area
/// panel) along its -Z.
fn light_icon(out: &mut Vec<LineVertex>, world: Mat4, l: &shotstage_core::LightData, c: [f32; 3]) {
    use shotstage_core::LightKind;
    let p = |x: f32, y: f32, z: f32| world.transform_point3(Vec3::new(x, y, z));
    let r = 0.08;
    for (a, b) in
        [(p(-r, 0.0, 0.0), p(r, 0.0, 0.0)), (p(0.0, -r, 0.0), p(0.0, r, 0.0)), (p(0.0, 0.0, -r), p(0.0, 0.0, r))]
    {
        line(out, a, b, c);
    }
    let ring = |out: &mut Vec<LineVertex>, rad: f32, z: f32| {
        for i in 0..16 {
            let (a0, a1) = (i as f32 / 16.0 * std::f32::consts::TAU, (i + 1) as f32 / 16.0 * std::f32::consts::TAU);
            line(out, p(a0.cos() * rad, a0.sin() * rad, z), p(a1.cos() * rad, a1.sin() * rad, z), c);
        }
    };
    match l.kind {
        LightKind::Point => ring(out, r * 1.6, 0.0),
        LightKind::Sun => {
            ring(out, r * 1.6, 0.0);
            line(out, p(0.0, 0.0, 0.0), p(0.0, 0.0, -0.8), c);
            line(out, p(0.0, 0.0, -0.8), p(0.05, 0.0, -0.68), c);
            line(out, p(0.0, 0.0, -0.8), p(-0.05, 0.0, -0.68), c);
        }
        LightKind::Spot => {
            let len = 0.6;
            let rad = (l.cone_degrees.to_radians() / 2.0).tan().min(4.0) * len;
            for a in [0.0f32, 90.0, 180.0, 270.0] {
                let a = a.to_radians();
                line(out, p(0.0, 0.0, 0.0), p(a.cos() * rad, a.sin() * rad, -len), c);
            }
            ring(out, rad, -len);
        }
        LightKind::Area => {
            let (w, h) = (l.size[0] / 2.0, l.size[1] / 2.0);
            let q = [p(-w, -h, 0.0), p(w, -h, 0.0), p(w, h, 0.0), p(-w, h, 0.0)];
            for i in 0..4 {
                line(out, q[i], q[(i + 1) % 4], c);
            }
            line(out, q[0], q[2], c);
            line(out, p(0.0, 0.0, 0.0), p(0.0, 0.0, -0.4), c);
        }
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn line(out: &mut Vec<LineVertex>, a: Vec3, b: Vec3, color: [f32; 3]) {
    out.push(LineVertex { pos: a.to_array(), color });
    out.push(LineVertex { pos: b.to_array(), color });
}

fn grid(out: &mut Vec<LineVertex>) {
    let n = 10;
    for i in -n..=n {
        let f = i as f32;
        let (cx, cz) = match i {
            0 => ([0.75, 0.25, 0.25], [0.25, 0.4, 0.8]),
            _ => ([0.32; 3], [0.32; 3]),
        };
        // x-axis line (red) runs along X at z=0; z-axis line (blue) along Z at x=0
        line(out, Vec3::new(-n as f32, 0.0, f), Vec3::new(n as f32, 0.0, f), cx);
        line(out, Vec3::new(f, 0.0, -n as f32), Vec3::new(f, 0.0, n as f32), cz);
    }
}

fn frustum(out: &mut Vec<LineVertex>, world: Mat4, hfov: f32, vfov: f32, c: [f32; 3]) {
    let d = 0.6;
    let (hx, hy) = ((hfov / 2.0).tan() * d, (vfov / 2.0).tan() * d);
    let p = |x: f32, y: f32, z: f32| world.transform_point3(Vec3::new(x, y, z));
    let o = p(0.0, 0.0, 0.0);
    let corners = [p(-hx, -hy, -d), p(hx, -hy, -d), p(hx, hy, -d), p(-hx, hy, -d)];
    for i in 0..4 {
        line(out, o, corners[i], c);
        line(out, corners[i], corners[(i + 1) % 4], c);
    }
    // "up" marker triangle above the frame
    let (a, b, t) = (p(-hx * 0.4, hy * 1.1, -d), p(hx * 0.4, hy * 1.1, -d), p(0.0, hy * 1.5, -d));
    line(out, a, b, c);
    line(out, b, t, c);
    line(out, t, a, c);
}

/// Pick the mesh entity under a world-space ray (closest triangle hit).
pub fn pick(scene: &Scene, frame: f32, origin: Vec3, dir: Vec3) -> Option<String> {
    spatial::raycast(scene, frame, origin, dir, &RayOptions::default())?.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotstage_core::{CameraData, Entity, MeshSource, Primitive, Transform};

    fn scene() -> Scene {
        let mut s = Scene::default();
        s.insert(Entity {
            id: "box".into(),
            name: "Box".into(),
            parent: None,
            transform: Transform::default(),
            visible: true,
            locked: false,
            kind: EntityKind::Mesh {
                source: MeshSource::Primitive { primitive: Primitive::Cube { size: [1.0; 3] } },
                modifiers: vec![],
                color: [0.5; 3],
                material: None,
            },
        })
        .unwrap();
        s.insert(Entity {
            id: "cam".into(),
            name: "Cam".into(),
            parent: None,
            transform: Transform { translation: [0.0, 0.0, 5.0], ..Default::default() },
            visible: true,
            locked: false,
            kind: EntityKind::Camera(CameraData::default()),
        })
        .unwrap();
        s
    }

    #[test]
    fn builds_triangles_and_overlays() {
        let s = scene();
        let g = build(&s, 0.0, &Overlay { grid: true, cameras: true, ..Default::default() });
        assert_eq!(g.triangles.len(), 36);
        assert!(g.lines.len() > 84);
        let mut hidden = s.clone();
        hidden.get_mut("box").unwrap().visible = false;
        assert!(build(&hidden, 0.0, &Overlay::default()).triangles.is_empty());
    }

    #[test]
    fn picking() {
        let s = scene();
        assert_eq!(pick(&s, 0.0, Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z).as_deref(), Some("box"));
        assert_eq!(pick(&s, 0.0, Vec3::new(3.0, 0.0, 5.0), Vec3::NEG_Z), None);
    }
}
