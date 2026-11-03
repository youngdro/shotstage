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
