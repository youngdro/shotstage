//! Starter scenes: a ready set with people, props, lights, cameras and shots in one step, built
//! from the same commands an agent would use (so the result is ordinary, editable content).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::Scene;

use crate::{CmdResult, Command, CommandError, command};

pub const TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "groceryAisle",
        "Fresh aisle",
        "A fresh-food aisle: produce stand, melons, chillers, a shelf, a shopper reaching and a crouching clerk; a slow push-in",
    ),
    (
        "productTabletop",
        "Product shot",
        "Infinity cove, stepped podium, hero bottle and jar with leaves and pebbles, softbox lighting; a low close-up with a 360 spin",
    ),
    (
        "packshot",
        "White packshot",
        "White cove, a drinks can on a plinth with ice, a tube on an acrylic riser, softbox and reflector; a hero arc then a detail push-in",
    ),
    (
        "flatLay",
        "Flat lay",
        "A breakfast table seen from straight above: tray, coffee, jar, citrus, flowers and a carton under a soft side light",
    ),
    (
        "interview",
        "Interview",
        "Two people sitting face to face with a lamp and a wall behind; three-point light; two over-the-shoulder cameras and a wide",
    ),
    (
        "streetCorner",
        "Street corner",
        "A car, trees and a doorway; a person walking past; a tracking shot and a drone flyover; golden-hour sun",
    ),
    ("cafe", "Cafe", "Tables and chairs, two people talking, a counter with a clerk; a gentle arc around them"),
    ("studio", "Empty studio", "Just an infinity cove, a camera and three-point light, ready for your own blocks"),
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TemplateParams {
    /// groceryAisle, productTabletop, packshot, flatLay, interview, streetCorner, cafe or studio.
    name: String,
    /// Clear the scene first. Default true.
    replace: Option<bool>,
}

fn steps(name: &str) -> Option<Vec<(&'static str, Value)>> {
    let floor = (
        "entity.create",
        json!({"type": "mesh", "id": "floor", "name": "floor", "primitive": {"shape": "plane", "size": [16, 16]}, "color": [0.62, 0.62, 0.6], "locked": true}),
    );
    let p = |kind: &str, id: &str, pos: [f32; 3], yaw: f32| {
        ("prefab.create", json!({"kind": kind, "id": id, "position": pos, "rotationY": yaw}))
    };
    let cam = |id: &str, name: &str| ("entity.create", json!({"type": "camera", "id": id, "name": name}));
    let frame = |cam: &str, subject: &str, size: &str, side: &str, angle: &str, mm: f32| {
        (
            "camera.frame",
            json!({"camera": cam, "subject": subject, "size": size, "side": side, "angle": angle, "focalLengthMm": mm, "makeActive": true}),
        )
    };
    let shot = |cam: &str, name: &str, a: u32, b: u32| {
        ("shot.create", json!({"camera": cam, "name": name, "start": a, "end": b}))
    };
    let motion = |preset: &str, target: &str, subject: Option<&str>, start: u32, secs: f32| {
        let mut v = json!({"preset": preset, "target": target, "start": start, "duration": secs});
        if let Some(s) = subject {
            v["subject"] = json!(s);
        }
        ("motion.apply", v)
    };
    let timeline = |end: u32| ("timeline.set", json!({"fps": 24, "start": 0, "end": end}));
    // a seated person: standing on the floor at the chair (not dropped onto the seat), hips over it
    let seated = |kind: &str, id: &str, pos: [f32; 3], yaw: f32| {
        let (s, c) = yaw.to_radians().sin_cos();
        let pos = [pos[0] + s * 0.06, 0.0, pos[2] + c * 0.06];
        [
            ("prefab.create", json!({"kind": kind, "id": id, "position": pos, "rotationY": yaw, "drop": false})),
            ("pose.set", json!({"id": id, "pose": "sit", "drop": false})),
        ]
    };
    Some(match name {
        "groceryAisle" => vec![
            floor,
            p("produceStand", "produce", [0.0, 0.0, -0.6], 0.0),
            p("watermelonBin", "melons", [-2.0, 0.0, -0.4], 0.0),
            p("openChiller", "chiller", [0.2, 0.0, -3.2], 0.0),
            p("openChiller", "chiller_2", [1.5, 0.0, -3.2], 0.0),
            p("gondolaShelf", "shelf", [-1.6, 0.0, -3.2], 0.0),
            p("priceSign", "sign", [1.5, 0.0, -1.5], 0.0),
            p("woman", "shopper", [0.3, 0.0, 1.0], 180.0),
            (
                "pose.set",
                json!({"id": "shopper", "pose": "reach", "joints": {"shoulder_r": [-70, 0, -6], "elbow_r": [-20, 0, 0]}}),
            ),
            p("basket", "basket", [0.65, 0.0, 1.0], 0.0),
            p("man", "clerk", [-2.0, 0.0, 0.6], 200.0),
            ("pose.set", json!({"id": "clerk", "pose": "crouch"})),
            p("cart", "cart", [2.2, 0.0, 1.2], 200.0),
            timeline(119),
            cam("cam_a", "Cam A"),
            frame("cam_a", "shopper", "full", "frontRight", "eyeLevel", 35.0),
            motion("pushIn", "cam_a", Some("shopper"), 0, 5.0),
            shot("cam_a", "push-in", 0, 119),
        ],
        "productTabletop" => vec![
            p("infinityCove", "cove", [0.0, 0.0, 0.0], 0.0),
            p("steppedPodium", "podium", [0.0, 0.0, 0.3], 0.0),
            p("pumpBottle", "hero", [-0.08, 1.0, 0.24], 0.0),
            p("jar", "jar", [0.12, 1.0, 0.42], 0.0),
            p("leafBranch", "leaves", [0.28, 0.0, 0.6], -20.0),
            p("pebbles", "pebbles", [-0.28, 0.0, 0.62], 0.0),
            p("softbox", "softbox_l", [1.3, 0.0, 1.6], -140.0),
            p("softbox", "softbox_r", [-1.3, 0.0, 1.6], 140.0),
            ("light.set", json!({"id": "softbox_l_light", "intensity": 0.8})),
            ("light.set", json!({"id": "softbox_r_light", "intensity": 0.5})),
            timeline(143),
            cam("cam_hero", "Hero"),
            frame("cam_hero", "podium", "full", "front", "low", 85.0),
            motion("productSpin", "cam_hero", Some("podium"), 0, 6.0),
            shot("cam_hero", "hero spin", 0, 143),
        ],
        "packshot" => vec![
            p("infinityCove", "cove", [0.0, 0.0, 0.0], 0.0),
            p("podium", "plinth", [0.0, 0.0, 0.35], 0.0),
            p("can", "hero", [0.0, 1.0, 0.35], 0.0),
            p("iceCubes", "ice", [0.2, 0.0, 0.6], 0.0),
            p("acrylicStand", "riser", [-0.38, 0.0, 0.5], 15.0),
            p("tube", "tube", [-0.38, 1.0, 0.5], 15.0),
            p("softbox", "softbox", [1.3, 0.0, 1.7], -140.0),
            p("reflector", "reflector", [-1.1, 0.0, 1.3], 140.0),
            ("light.set", json!({"id": "softbox_light", "intensity": 0.9})),
            timeline(143),
            cam("cam_hero", "Hero"),
            frame("cam_hero", "hero", "wide", "front", "high", 85.0),
            motion("orbitLeft", "cam_hero", Some("plinth"), 0, 3.0),
            shot("cam_hero", "hero arc", 0, 71),
            cam("cam_detail", "Detail"),
            frame("cam_detail", "hero", "full", "frontRight", "eyeLevel", 100.0),
            ("motion.apply", json!({"preset": "pushIn", "target": "cam_detail", "subject": "hero", "start": 72, "duration": 3.0, "intensity": 0.5})),
            shot("cam_detail", "detail", 72, 143),
        ],
        "flatLay" => vec![
            floor,
            p("table", "table", [0.0, 0.0, 0.0], 0.0),
            p("tray", "tray", [0.05, 1.2, 0.02], 8.0),
            p("cup", "coffee", [0.3, 1.2, -0.18], 0.0),
            p("jar", "jar", [-0.32, 1.2, 0.14], 0.0),
            p("citrusSlice", "orange", [0.0, 1.2, 0.25], 0.0),
            p("citrusSlice", "orange_2", [0.1, 1.2, 0.29], 40.0),
            p("flower", "flower", [-0.4, 1.2, -0.22], 30.0),
            p("leafBranch", "leaves", [0.42, 1.2, 0.24], -40.0),
            p("carton", "carton", [-0.08, 1.2, -0.22], -12.0),
            p("softbox", "softbox", [1.7, 0.0, 0.9], -115.0),
            ("light.set", json!({"id": "softbox_light", "intensity": 0.8})),
            timeline(119),
            cam("cam_top", "Top"),
            frame("cam_top", "table", "full", "front", "birdsEye", 50.0),
            motion("topDown", "cam_top", Some("table"), 0, 4.0),
            shot("cam_top", "flat lay", 0, 119),
        ],
        "interview" => vec![
            floor,
            p("wall", "wall", [0.0, 0.0, -2.2], 0.0),
            p("chair", "chair_a", [-0.9, 0.0, 0.0], 90.0),
            p("chair", "chair_b", [0.9, 0.0, 0.0], -90.0),
            seated("man", "host", [-0.9, 0.0, 0.0], 90.0)[0].clone(),
            seated("man", "host", [-0.9, 0.0, 0.0], 90.0)[1].clone(),
            seated("woman", "guest", [0.9, 0.0, 0.0], -90.0)[0].clone(),
            seated("woman", "guest", [0.9, 0.0, 0.0], -90.0)[1].clone(),
            p("lamp", "lamp", [-1.8, 0.0, -1.5], 0.0),
            p("tree", "plant", [2.0, 0.0, -1.7], 0.0),
            ("entity.setTransform", json!({"id": "plant", "scale": [0.4, 0.4, 0.4]})),
            ("entity.place", json!({"id": "plant"})),
            timeline(239),
            cam("cam_wide", "Wide"),
            frame("cam_wide", "wall", "wide", "front", "eyeLevel", 28.0),
            cam("cam_host", "Over guest"),
            frame("cam_host", "host", "mediumCloseUp", "frontLeft", "eyeLevel", 65.0),
            cam("cam_guest", "Over host"),
            frame("cam_guest", "guest", "mediumCloseUp", "frontRight", "eyeLevel", 65.0),
            ("light.preset", json!({"preset": "threePoint", "camera": "cam_wide"})),
            shot("cam_wide", "wide", 0, 71),
            shot("cam_host", "host", 72, 155),
            shot("cam_guest", "guest", 156, 239),
            motion("talk", "host", None, 72, 3.5),
            motion("talk", "guest", None, 156, 3.5),
        ],
        "streetCorner" => vec![
            floor,
            p("doorway", "building", [0.0, 0.0, -3.0], 0.0),
            p("wall", "building_2", [4.1, 0.0, -3.0], 0.0),
            p("car", "car", [2.5, 0.0, 1.5], 90.0),
            p("tree", "tree", [-3.0, 0.0, -1.5], 0.0),
            p("tree", "tree_2", [5.0, 0.0, -1.5], 0.0),
            p("lamp", "street_lamp", [-1.5, 0.0, 0.5], 0.0),
            p("man", "walker", [-3.0, 0.0, 0.0], 90.0),
            timeline(143),
            motion("walkCycle", "walker", None, 0, 6.0),
            cam("cam_track", "Tracking"),
            frame("cam_track", "walker", "full", "frontRight", "eyeLevel", 35.0),
            motion("tracking", "cam_track", Some("walker"), 0, 6.0),
            cam("cam_drone", "Drone"),
            frame("cam_drone", "building", "wide", "front", "high", 24.0),
            motion("droneFlyover", "cam_drone", Some("walker"), 72, 3.0),
            ("light.preset", json!({"preset": "goldenHour", "camera": "cam_track"})),
            shot("cam_track", "tracking", 0, 71),
            shot("cam_drone", "drone", 72, 143),
        ],
        "cafe" => vec![
            floor,
            p("wall", "wall", [0.0, 0.0, -3.0], 0.0),
            p("table", "table", [0.0, 0.0, 0.0], 0.0),
            p("chair", "chair_a", [-0.75, 0.0, 0.0], 90.0),
            p("chair", "chair_b", [0.75, 0.0, 0.0], -90.0),
            seated("woman", "friend_a", [-0.75, 0.0, 0.0], 90.0)[0].clone(),
            seated("woman", "friend_a", [-0.75, 0.0, 0.0], 90.0)[1].clone(),
            seated("man", "friend_b", [0.75, 0.0, 0.0], -90.0)[0].clone(),
            seated("man", "friend_b", [0.75, 0.0, 0.0], -90.0)[1].clone(),
            p("cup", "cup_a", [-0.25, 1.0, 0.1], 0.0),
            p("cup", "cup_b", [0.25, 1.0, -0.1], 0.0),
            p("checkout", "counter", [2.6, 0.0, -1.8], -90.0),
            p("man", "barista", [3.3, 0.0, -1.8], -90.0),
            p("table", "table_2", [-2.4, 0.0, -1.2], 0.0),
            p("chair", "chair_c", [-2.4, 0.0, -0.4], 180.0),
            p("lamp", "lamp", [-3.2, 0.0, -2.4], 0.0),
            timeline(143),
            motion("talk", "friend_a", None, 0, 6.0),
            cam("cam_a", "Cam A"),
            frame("cam_a", "table", "wide", "frontLeft", "high", 35.0),
            motion("orbitRight", "cam_a", Some("table"), 0, 6.0),
            ("light.preset", json!({"preset": "daylight", "camera": "cam_a"})),
            shot("cam_a", "arc", 0, 143),
        ],
        "studio" => vec![
            p("infinityCove", "cove", [0.0, 0.0, 0.0], 0.0),
            timeline(119),
            ("entity.create", json!({"type": "camera", "id": "cam_a", "name": "Cam A", "position": [0, 1.4, 4.5]})),
            ("camera.lookAt", json!({"id": "cam_a", "target": [0, 0.9, 0]})),
            ("camera.setActive", json!({"id": "cam_a"})),
            ("light.preset", json!({"preset": "threePoint", "subject": "cove", "camera": "cam_a"})),
            shot("cam_a", "shot 1", 0, 119),
        ],
        _ => return None,
    })
}

fn apply(scene: &mut Scene, p: TemplateParams) -> CmdResult<Value> {
    let Some(steps) = steps(&p.name) else {
        return Err(CommandError::invalid(format!(
            "no template `{}`; use one of {}",
            p.name,
            TEMPLATES.iter().map(|t| t.0).collect::<Vec<_>>().join(", ")
        )));
    };
    if p.replace.unwrap_or(true) {
        *scene = Scene::default();
    }
    let registry = crate::registry();
    for (i, (cmd, params)) in steps.into_iter().enumerate() {
        let c = registry
            .iter()
            .find(|c| c.id == cmd)
            .ok_or_else(|| CommandError::new("failed", format!("no command {cmd}")))?;
        (c.run)(scene, params)
            .map_err(|e| CommandError::new(&e.code, format!("template step {i} ({cmd}): {}", e.message)))?;
    }
    Ok(json!({ "template": p.name, "entities": scene.entities.len(), "shots": scene.timeline.shots.len() }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("scene.template", "Start from a ready scene: groceryAisle, productTabletop, interview, streetCorner, cafe or studio (people, props, lights, cameras, shots and moves; all editable). Clears the scene first unless replace is false.", mutates: true, TemplateParams, apply),
        command!("scene.templates", "The starter scenes scene.template can build.", mutates: false, crate::builtin::NoParams,
            |_, _| Ok(json!(TEMPLATES.iter().map(|(k, l, d)| json!({ "name": k, "label": l, "description": d })).collect::<Vec<_>>()))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_builds_and_undoes_in_one_step() {
        for (name, ..) in TEMPLATES {
            let mut s = crate::Session::default();
            let r =
                s.dispatch("scene.template", json!({"name": name})).unwrap_or_else(|e| panic!("{name}: {e}")).result;
            assert!(r["entities"].as_u64().unwrap() > 3, "{name}: {r}");
            assert!(r["shots"].as_u64().unwrap() >= 1, "{name}");
            s.dispatch("history.undo", json!({})).unwrap();
            assert!(s.scene().entities.is_empty(), "{name}");
        }
    }
}
