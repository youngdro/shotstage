use serde_json::json;

use super::*;

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.dispatch(id, p).unwrap_or_else(|e| panic!("{id}: {e}")).result
}

#[test]
fn every_command_has_a_schema_and_unique_id() {
    let s = Session::default();
    let list = s.list();
    let mut ids: Vec<_> = list.iter().map(|c| c.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), list.len());
    for c in &list {
        assert!(c.params.is_object(), "{} has no schema", c.id);
    }
}

#[test]
fn create_edit_undo_redo() {
    let mut s = Session::default();
    let id = run(
        &mut s,
        "entity.create",
        json!({"type": "mesh", "primitive": {"shape": "sphere", "radius": 0.5, "segments": 16, "rings": 8}}),
    );
    assert_eq!(id["id"], "sphere_1");
    run(&mut s, "entity.setTransform", json!({"id": "sphere_1", "position": [1, 2, 3]}));
    assert_eq!(s.scene().get("sphere_1").unwrap().transform.translation, [1.0, 2.0, 3.0]);
    run(&mut s, "history.undo", json!({}));
    assert_eq!(s.scene().get("sphere_1").unwrap().transform.translation, [0.0; 3]);
    run(&mut s, "history.redo", json!({}));
    assert_eq!(s.scene().get("sphere_1").unwrap().transform.translation, [1.0, 2.0, 3.0]);
    assert!(s.revision() >= 4);
}

#[test]
fn errors_leave_scene_untouched() {
    let mut s = Session::default();
    let before = s.scene().clone();
    for (id, p) in [
        ("entity.create", json!({"type": "mesh", "primitive": {"shape": "cube", "size": [-1, 1, 1]}})),
        ("entity.create", json!({"type": "rocket"})),
        ("entity.create", json!({"type": "mesh", "bogus": 1})),
        ("entity.setTransform", json!({"id": "nope", "position": [0, 0, 0]})),
        ("mesh.addModifier", json!({"id": "nope", "modifier": {"type": "subdivide", "levels": 9, "smooth": true}})),
        ("timeline.set", json!({"fps": 0})),
        ("no.such", json!({})),
        ("entity.create", json!({"type": "mesh", "position": [1e30, 0, 0]})),
        ("entity.create", json!({"type": "mesh", "id": "bad id!"})),
    ] {
        assert!(s.dispatch(id, p.clone()).is_err(), "{id} {p} should fail");
    }
    assert_eq!(s.scene(), &before);
    assert!(s.dispatch("history.undo", json!({})).is_err());
}

#[test]
fn batch_is_atomic_and_one_undo_step() {
    let mut s = Session::default();
    let ok = s
        .dispatch_json(json!({"batch": [
            {"command": "entity.create", "params": {"type": "mesh", "id": "a"}},
            {"command": "entity.create", "params": {"type": "mesh", "id": "b", "position": [2, 0, 0]}},
        ]}))
        .unwrap();
    assert!(ok.changed);
    assert_eq!(s.scene().entities.len(), 2);
    let err = s
        .dispatch_json(json!({"batch": [
            {"command": "entity.create", "params": {"type": "mesh", "id": "c"}},
            {"command": "entity.delete", "params": {"id": "zzz"}},
        ]}))
        .unwrap_err();
    assert!(err.message.contains("step 1"));
    assert_eq!(s.scene().entities.len(), 2);
    run(&mut s, "history.undo", json!({}));
    assert!(s.scene().entities.is_empty());
}

#[test]
fn modifiers_join_and_export() {
    let mut s = Session::default();
    run(&mut s, "entity.create", json!({"type": "mesh", "id": "a"}));
    run(&mut s, "entity.create", json!({"type": "mesh", "id": "b", "position": [3, 0, 0]}));
    run(&mut s, "mesh.addModifier", json!({"id": "a", "modifier": {"type": "subdivide", "levels": 1, "smooth": true}}));
    assert_eq!(run(&mut s, "mesh.stats", json!({"id": "a"}))["faces"], 24);
    run(
        &mut s,
        "mesh.addModifier",
        json!({"id": "a", "modifier": {"type": "array", "count": 2, "offset": [1.5, 0, 0]}}),
    );
    run(&mut s, "mesh.applyModifiers", json!({"id": "a"}));
    assert_eq!(run(&mut s, "mesh.stats", json!({"id": "a"}))["faces"], 48);
    run(&mut s, "mesh.join", json!({"ids": ["a", "b"]}));
    assert!(!s.scene().contains("b"));
    let stats = run(&mut s, "mesh.stats", json!({"id": "a"}));
    assert_eq!(stats["faces"], 54);
    assert_eq!(stats["bounds"]["max"][0], 3.5);
    let obj = run(&mut s, "mesh.exportObj", json!({"id": "a"}));
    assert!(obj["obj"].as_str().unwrap().contains("\nf "));
}

#[test]
fn camera_moves_and_describe() {
    let mut s = Session::default();
    run(
        &mut s,
        "entity.create",
        json!({"type": "mesh", "id": "actor", "primitive": {"shape": "cube", "size": [0.5, 1.7, 0.3]}, "position": [0, 0.85, 0]}),
    );
    run(&mut s, "entity.create", json!({"type": "camera", "id": "cam", "position": [0, 3.5, 5]}));
    assert_eq!(s.scene().active_camera.as_deref(), Some("cam"));
    run(&mut s, "camera.lookAt", json!({"id": "cam", "target": "actor"}));
    let d = run(&mut s, "camera.describe", json!({"subject": "actor"}));
    assert_eq!(d["angle"], "high angle");
    run(&mut s, "camera.dollyMove", json!({"id": "cam", "distance": 2.5, "start": 0, "end": 48}));
    run(&mut s, "camera.orbitMove", json!({"id": "cam", "target": "actor", "degrees": 90, "start": 48, "end": 96}));
    let traj = run(&mut s, "camera.trajectory", json!({"start": 0, "end": 96}));
    assert_eq!(traj["frames"].as_array().unwrap().len(), 97);
    let close = run(&mut s, "camera.describe", json!({"frame": 48, "subject": "actor"}));
    assert_ne!(close["shotSize"], d["shotSize"]);
    run(&mut s, "shot.create", json!({"camera": "cam", "start": 0, "end": 96}));
    run(&mut s, "keyframe.set", json!({"id": "cam", "property": "focalLength", "frame": 0}));
    run(&mut s, "camera.setLens", json!({"id": "cam", "focalLengthMm": 85}));
    run(&mut s, "keyframe.set", json!({"id": "cam", "property": "focalLength", "frame": 96}));
    let tracks = run(&mut s, "keyframe.list", json!({"id": "cam"}));
    assert_eq!(tracks.as_array().unwrap().len(), 2);
}

#[test]
fn scene_round_trips_through_json() {
    let mut s = Session::default();
    run(
        &mut s,
        "entity.create",
        json!({"type": "mesh", "primitive": {"shape": "torus", "majorRadius": 1, "minorRadius": 0.2, "majorSegments": 24, "minorSegments": 8}}),
    );
    run(&mut s, "entity.create", json!({"type": "camera"}));
    let doc = run(&mut s, "scene.inspect", json!({}));
    let mut t = Session::default();
    run(&mut t, "scene.load", json!({"scene": doc}));
    assert_eq!(s.scene(), t.scene());
}

#[test]
fn merged_edits_share_one_undo_step() {
    let mut s = Session::default();
    run(&mut s, "entity.create", json!({"type": "mesh", "id": "a"}));
    for x in 1..=5 {
        s.dispatch_json(
            json!({"command": "entity.translate", "params": {"id": "a", "delta": [1, 0, 0]}, "merge": "drag-1"}),
        )
        .unwrap();
        let _ = x;
    }
    assert_eq!(s.scene().get("a").unwrap().transform.translation[0], 5.0);
    run(&mut s, "history.undo", json!({}));
    assert_eq!(s.scene().get("a").unwrap().transform.translation[0], 0.0);
    run(&mut s, "history.undo", json!({}));
    assert!(s.scene().entities.is_empty());
}

#[test]
fn blocks_rest_on_each_other() {
    let mut s = Session::default();
    let table = run(&mut s, "prefab.create", json!({"kind": "table", "position": [2, 5, 0]}));
    assert_eq!(table["id"], "table_1");
    // dropped from 5 m onto the floor
    let b = run(&mut s, "entity.bounds", json!({"id": "table_1"}));
    assert!(b["min"][1].as_f64().unwrap().abs() < 1e-4, "{b}");
    assert!((b["max"][1].as_f64().unwrap() - 0.75).abs() < 1e-4);
    // a crate on the table top, then a lamp next to the table, seen from a camera
    run(&mut s, "prefab.create", json!({"kind": "crate", "id": "box"}));
    let r = run(&mut s, "entity.place", json!({"id": "box", "on": "table_1"}));
    assert_eq!(r["restingOn"], "table_1_top");
    assert!((r["position"][1].as_f64().unwrap() - 0.75).abs() < 1e-4, "{r}");
    run(&mut s, "entity.create", json!({"type": "camera", "id": "cam", "position": [2, 1.6, 6]}));
    run(&mut s, "camera.lookAt", json!({"id": "cam", "target": "table_1"}));
    run(&mut s, "prefab.create", json!({"kind": "lamp", "id": "lamp"}));
    let r = run(
        &mut s,
        "entity.place",
        json!({"id": "lamp", "nextTo": "table_1", "side": "left", "view": "cam", "gap": 0.1}),
    );
    assert!(r["position"][0].as_f64().unwrap() < 1.3, "lamp left of the table on screen: {r}");
    // drag: a ray from above the table lands on its top; the same merge key is one undo step
    for x in [1.8, 2.0, 2.2] {
        let req = json!({"command": "entity.place", "merge": "drag-1",
            "params": {"id": "box", "ray": {"origin": [x, 3, 0], "direction": [0, -1, 0]}, "snap": 0.1}});
        s.dispatch_json(req).unwrap();
    }
    let p = run(&mut s, "entity.get", json!({"id": "box"}))["transform"]["translation"].clone();
    assert!((p[0].as_f64().unwrap() - 2.2).abs() < 1e-4 && (p[1].as_f64().unwrap() - 0.75).abs() < 1e-4, "{p}");
    run(&mut s, "history.undo", json!({}));
    let p = run(&mut s, "entity.get", json!({"id": "box"}))["transform"]["translation"].clone();
    assert!((p[0].as_f64().unwrap() - 2.0).abs() < 1e-3, "drag undone in one step: {p}");
    // bad combinations are rejected without changing anything
    let rev = s.revision();
    assert!(s.dispatch("entity.place", json!({"id": "box", "on": "table_1", "nextTo": "lamp"})).is_err());
    assert!(s.dispatch("entity.place", json!({"id": "table_1", "on": "table_1_top"})).is_err());
    assert_eq!(s.revision(), rev);
}

#[test]
fn frame_by_shot_size_and_turn() {
    let mut s = Session::default();
    run(&mut s, "prefab.create", json!({"kind": "person", "id": "hero"}));
    let r = run(&mut s, "camera.frame", json!({"subject": "hero", "size": "medium", "side": "frontLeft"}));
    assert_eq!(r["camera"], "camera_1", "a camera is created when there is none");
    assert_eq!(r["describe"]["shotSize"], "medium shot");
    assert_eq!(s.scene().active_camera.as_deref(), Some("camera_1"));
    let r = run(&mut s, "camera.frame", json!({"subject": "hero", "size": "wide", "angle": "high"}));
    assert_eq!(r["describe"]["shotSize"], "wide shot");
    assert_eq!(r["describe"]["angle"], "high angle");
    run(&mut s, "entity.face", json!({"id": "hero", "target": [5, 0, 0]}));
    let e = run(&mut s, "entity.get", json!({"id": "hero"}));
    let rot = glam::Quat::from_array(serde_json::from_value(e["transform"]["rotation"].clone()).unwrap());
    assert!((rot * glam::Vec3::Z - glam::Vec3::X).length() < 1e-4, "{e}");
    run(&mut s, "entity.rotate", json!({"id": "hero", "degrees": 90}));
    let hit = run(&mut s, "scene.raycast", json!({"origin": [0, 1.6, 3], "direction": [0, 0, -1]}));
    assert_eq!(hit["id"], "hero_skull", "the head mesh hangs off the head joint");
    assert!(run(&mut s, "prefab.list", json!({})).as_array().unwrap().len() >= 10);
}

#[test]
fn duplicate_copies_groups() {
    let mut s = Session::default();
    run(&mut s, "prefab.create", json!({"kind": "chair", "id": "chair"}));
    let r = run(&mut s, "entity.duplicate", json!({"id": "chair", "offset": [1, 0, 0]}));
    assert_eq!(r["id"], "chair_1");
    let n = s.scene().entities.len();
    assert_eq!(n, 2 * 7);
    assert_eq!(s.scene().get("chair_1_seat").unwrap().parent.as_deref(), Some("chair_1"));
    let again = run(&mut s, "entity.duplicate", json!({"id": "chair"}));
    assert_eq!(s.scene().entities.len(), 3 * 7, "{again}");
}

fn world_y(s: &Session, id: &str) -> f32 {
    shotstage_core::eval::world_at(s.scene(), id, 0.0).unwrap().w_axis.y
}

#[test]
fn people_are_rigged_and_posable() {
    let mut s = Session::default();
    run(&mut s, "prefab.create", json!({"kind": "woman", "id": "ann"}));
    let b = run(&mut s, "entity.bounds", json!({"id": "ann"}));
    let h = b["size"][1].as_f64().unwrap();
    assert!((1.55..1.72).contains(&h), "a woman is about 1.65 m: {h}");
    assert!(b["min"][1].as_f64().unwrap().abs() < 0.01, "feet on the ground");
    // the arm hangs off the shoulder: raising the left arm sideways lifts the hand
    let hand = world_y(&s, "ann_wrist_l");
    run(&mut s, "pose.set", json!({"id": "ann_forearm_l", "joints": {"shoulder_l": [0, 0, 90]}, "drop": false}));
    assert!(world_y(&s, "ann_wrist_l") > hand + 0.4);
    // sitting lowers the hips but keeps the feet on the floor
    run(&mut s, "pose.set", json!({"id": "ann", "pose": "sit"}));
    assert!(world_y(&s, "ann_pelvis") < 0.65, "sitting: {}", world_y(&s, "ann_pelvis"));
    let b = run(&mut s, "entity.bounds", json!({"id": "ann"}));
    assert!(b["min"][1].as_f64().unwrap().abs() < 0.01, "{b}");
    // standing again restores the rest height, and one undo step reverts a pose
    run(&mut s, "pose.set", json!({"id": "ann", "pose": "stand"}));
    assert!(world_y(&s, "ann_pelvis") > 0.85);
    run(&mut s, "history.undo", json!({}));
    assert!(world_y(&s, "ann_pelvis") < 0.65);
    // aim the right arm straight forward
    run(&mut s, "pose.set", json!({"id": "ann", "pose": "stand"}));
    let sh = shotstage_core::eval::world_at(s.scene(), "ann_shoulder_r", 0.0).unwrap().w_axis.truncate();
    run(
        &mut s,
        "pose.aim",
        json!({"id": "ann_upper_arm_r", "target": (sh + glam::Vec3::new(0.0, 0.0, 2.0)).to_array()}),
    );
    let el = shotstage_core::eval::world_at(s.scene(), "ann_elbow_r", 0.0).unwrap().w_axis.truncate();
    let dir = (el - sh).normalize();
    assert!(dir.z > 0.99, "arm points forward: {dir}");
    // hair
    let r = run(&mut s, "person.setHair", json!({"id": "ann", "style": "ponytail"}));
    assert_eq!(r["parts"].as_array().unwrap().len(), 2);
    run(&mut s, "person.setHair", json!({"id": "ann", "style": "none"}));
    assert!(!s.scene().contains("ann_hair"));
    assert!(s.dispatch("pose.set", json!({"id": "ann", "pose": "moonwalk"})).is_err());
    assert!(s.dispatch("pose.set", json!({"id": "ann", "joints": {"tail": [0, 0, 0]}})).is_err());
    run(&mut s, "prefab.create", json!({"kind": "table", "id": "t"}));
    assert!(s.dispatch("pose.set", json!({"id": "t", "pose": "sit"})).is_err());
    // a duplicated person stays posable
    let r = run(&mut s, "entity.duplicate", json!({"id": "ann"}));
    run(&mut s, "pose.set", json!({"id": r["id"], "pose": "wave"}));
    let list = run(&mut s, "pose.list", json!({}));
    assert!(list["poses"].as_array().unwrap().len() >= 10);
}

#[test]
fn product_shots_frame_small_subjects() {
    let mut s = Session::default();
    run(&mut s, "prefab.create", json!({"kind": "productTable", "id": "table"}));
    run(&mut s, "prefab.create", json!({"kind": "bottle", "id": "hero", "position": [0, 2, 0]}));
    let b = run(&mut s, "entity.bounds", json!({"id": "hero"}));
    assert!((b["min"][1].as_f64().unwrap() - 0.75).abs() < 0.01, "the bottle lands on the table: {b}");
    // match a real product's size
    run(
        &mut s,
        "prefab.create",
        json!({"kind": "carton", "id": "box", "size": [0.1, 0.3, 0.08], "position": [3, 0, 0]}),
    );
    let b = run(&mut s, "entity.bounds", json!({"id": "box"}));
    assert!(
        (b["size"][1].as_f64().unwrap() - 0.3).abs() < 1e-3 && (b["size"][0].as_f64().unwrap() - 0.1).abs() < 1e-3,
        "{b}"
    );
    for (size, word) in [("closeUp", "close-up"), ("medium", "medium shot"), ("full", "full shot")] {
        let r =
            run(&mut s, "camera.frame", json!({"subject": "hero", "size": size, "angle": "low", "focalLengthMm": 100}));
        assert_eq!(r["describe"]["shotSize"], word, "{r}");
        assert_eq!(r["describe"]["angle"], "low angle", "{r}");
    }
    // a low, wide subject is sized by its width: a wide shot shows all of it
    run(&mut s, "prefab.create", json!({"kind": "steppedPodium", "id": "pod", "position": [-3, 0, 0]}));
    for (size, word) in [("wide", "wide shot"), ("full", "full shot"), ("medium", "medium shot")] {
        let r = run(&mut s, "camera.frame", json!({"subject": "pod", "size": size, "angle": "high"}));
        assert_eq!(r["describe"]["shotSize"], word, "{r}");
    }
    for p in prefabs::PREFABS {
        run(&mut s, "prefab.create", json!({"kind": p.key, "position": [10, 0, 10]}));
    }
}
