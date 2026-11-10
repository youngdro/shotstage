//! Timing of the editor's hot paths on a big scene. Ignored by default (timings depend on the
//! machine); run with `cargo test --release -p shotstage-cli --test perf -- --ignored --nocapture`.

use std::time::Instant;

use serde_json::json;
use shotstage_commands::Session;
use shotstage_render::geometry::{self, Overlay};

/// The fresh aisle template copied a few times, a dozen people, and editable dense meshes.
fn big_scene() -> Session {
    let mut s = Session::default();
    s.dispatch("scene.template", json!({"name": "groceryAisle"})).unwrap();
    let roots: Vec<String> = s.scene().entities.iter().filter(|e| e.parent.is_none()).map(|e| e.id.clone()).collect();
    for _ in 0..4 {
        for r in &roots {
            let _ = s.dispatch("entity.duplicate", json!({"id": r}));
        }
    }
    for i in 0..12 {
        let kind = if i % 2 == 0 { "woman" } else { "man" };
        s.dispatch("prefab.create", json!({"kind": kind, "position": [i as f32 * 0.8 - 4.0, 0.0, 3.0]})).unwrap();
    }
    for i in 0..6 {
        let id = format!("dense_{i}");
        s.dispatch(
            "entity.create",
            json!({"id": id, "type": "mesh", "position": [i as f32, 1.0, 5.0],
                   "primitive": {"shape": "sphere", "radius": 0.4, "segments": 256, "rings": 128}}),
        )
        .unwrap();
        s.dispatch("mesh.makeEditable", json!({"id": id})).unwrap();
    }
    s
}

fn time<T>(label: &str, n: u32, mut f: impl FnMut() -> T) -> f64 {
    let _ = f();
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(f());
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("{label:<40} {ms:>9.3} ms");
    ms
}

#[test]
#[ignore]
fn big_scene_timings() {
    let mut s = big_scene();
    let scene = s.scene().clone();
    let tris: usize = geometry::build(&scene, 0.0, &Overlay::default()).triangles.len() / 3;
    println!("entities: {}, triangles: {tris}", scene.entities.len());
    time("scene clone", 20, || s.scene().clone());
    time("scene == clone", 20, || *s.scene() == scene);
    time("serialize (autosave)", 5, || serde_json::to_string(s.scene()).unwrap().len());
    time("query (mesh.stats)", 20, || s.dispatch("mesh.stats", json!({"id": "dense_0"})).unwrap());
    let mut x = 0.0;
    time("drag step (merged setTransform)", 20, || {
        x += 0.01;
        let req = json!({"command": "entity.translate", "params": {"id": "dense_0", "delta": [0.01, 0.0, 0.0]}, "merge": "drag"});
        s.dispatch_json(req).unwrap()
    });
    time("vertex drag (mesh.transformElements)", 10, || {
        s.dispatch(
            "mesh.transformElements",
            json!({"id": "dense_1", "vertices": [0, 1, 2], "translate": [0.0, 0.001, 0.0]}),
        )
        .unwrap()
    });
    let scene = s.scene().clone();
    time("geometry::build (one frame)", 10, || geometry::build(&scene, 0.0, &Overlay::default()).triangles.len());
    let mut g = geometry::Geometry::default();
    time("geometry::build_into (reused)", 10, || {
        geometry::build_into(&mut g, &scene, 0.0, &Overlay::default());
        g.triangles.len()
    });
    let mut f = 0.0;
    time("geometry::build (playing)", 10, || {
        f += 1.0;
        geometry::build(&scene, f, &Overlay::default()).triangles.len()
    });
    let mb = |s: &Session| serde_json::to_string(s.scene()).unwrap().len() as f64 / 1e6;
    println!("scene JSON: {:.1} MB", mb(&s));
}

#[test]
#[ignore]
fn build_breakdown() {
    use shotstage_core::eval::{posed_tris, world_at};
    use shotstage_core::spatial::is_hidden;
    let s = big_scene();
    let scene = s.scene().clone();
    time("is_hidden all", 5, || scene.entities.iter().filter(|e| is_hidden(&scene, &e.id)).count());
    time("world_at all", 5, || scene.entities.iter().filter_map(|e| world_at(&scene, &e.id, 0.0).ok()).count());
    time("posed_tris all", 5, || scene.entities.iter().filter_map(|e| posed_tris(&scene, &e.id, 0.0).ok()).count());
    let tris: Vec<_> = scene.entities.iter().filter_map(|e| posed_tris(&scene, &e.id, 0.0).ok()).collect();
    time("transform verts", 5, || {
        let mut out = Vec::new();
        let m = glam::Mat4::from_rotation_y(0.3);
        for t in &tris {
            for &i in &t.indices {
                out.push(m.transform_point3(glam::Vec3::from_array(t.positions[i as usize])));
                out.push(m.transform_vector3(glam::Vec3::from_array(t.normals[i as usize])).normalize_or_zero());
            }
        }
        out.len()
    });
    println!("tracks: {}", scene.timeline.tracks.len());
}

#[test]
#[ignore]
fn build_only() {
    let s = big_scene();
    let scene = s.scene().clone();
    std::hint::black_box(geometry::build(&scene, 0.0, &Overlay::default()));
    measured(&scene);
}

#[inline(never)]
fn measured(scene: &shotstage_core::Scene) {
    for _ in 0..3 {
        std::hint::black_box(geometry::build(scene, 0.0, &Overlay::default()));
    }
}
