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
    (
        "checkout",
        "Checkout",
        "A checkout counter: the cashier scans and chats while a shopper pays by phone; a medium two-shot then a close-up on the payment",
    ),
    (
        "freshCloseUp",
        "Fresh close-up",
        "Salmon on a cutting board with lemon, tomatoes, broccoli and a knife under a big soft light; straight down, then a low push-in on the salmon",
    ),
    (
        "darkHero",
        "Dark rim light",
        "A bottle on a black plinth in the dark: two strip lights behind trace its edges, a soft spot from above, a glow on the wall behind; a slow arc, then a push-in on the label",
    ),
    (
        "highKeyWhite",
        "High-key white",
        "Skincare set on white cubes in a white cove: a big soft overhead light, side strips, faint shadows; a slide across, then a detail push-in",
    ),
    (
        "goldenWindow",
        "Golden window light",
        "A bottle and a jar on a stone block in low warm sun, leaf shadows falling across them, cool fill; a slow push-in",
    ),
    (
        "pastelPop",
        "Pastel pop",
        "A drinks can on stacked pastel podiums with spheres and a cube, soft key light and pink and blue gels; an arc around it",
    ),
    (
        "iceDrink",
        "Ice-cold drink",
        "A can on ice on a deep blue set, cyan strip lights behind, a blue glow on the wall; low angle, push-in",
    ),
    (
        "macroFruit",
        "Macro fruit",
        "Strawberries, grapes and lemon on dark slate, warm backlight and soft side light; a 100 mm close lens sliding sideways",
    ),
    ("cafe", "Cafe", "Tables and chairs, two people talking, a counter with a clerk; a gentle arc around them"),
    ("studio", "Empty studio", "Just an infinity cove, a camera and three-point light, ready for your own blocks"),
];

/// The product close-ups among [`TEMPLATES`]: styled sets with tuned lights (the editor lists
/// them under their own heading).
pub const PRODUCT_LOOKS: &[&str] = &[
    "productTabletop",
    "packshot",
    "darkHero",
    "highKeyWhite",
    "goldenWindow",
    "pastelPop",
    "iceDrink",
    "macroFruit",
    "freshCloseUp",
];

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TemplateParams {
    /// groceryAisle, productTabletop, packshot, flatLay, interview, streetCorner, checkout,
    /// freshCloseUp, darkHero, highKeyWhite, goldenWindow, pastelPop, iceDrink, macroFruit, cafe or
    /// studio.
    name: String,
    /// Clear the scene first. Default true.
    replace: Option<bool>,
}

fn steps(name: &str) -> Option<Vec<(&'static str, Value)>> {
    if let Some(s) = product_look(name) {
        return Some(s);
    }
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
            p("humanShopper", "shopper", [0.3, 0.0, 1.0], 180.0),
            (
                "pose.set",
                json!({"id": "shopper", "pose": "reach", "joints": {"shoulder_r": [-70, 0, -6], "elbow_r": [-20, 0, 0]}}),
            ),
            ("person.setExpression", json!({"id": "shopper", "expression": "smile", "amount": 0.6})),
            p("basket", "basket", [0.65, 0.0, 1.0], 0.0),
            p("humanStaff", "clerk", [-2.0, 0.0, 0.6], 200.0),
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
            motion("orbitLeft", "cam_hero", Some("hero"), 0, 3.0),
            shot("cam_hero", "hero arc", 0, 71),
            cam("cam_detail", "Detail"),
            frame("cam_detail", "hero", "full", "frontRight", "eyeLevel", 100.0),
            (
                "motion.apply",
                json!({"preset": "pushIn", "target": "cam_detail", "subject": "hero", "start": 72, "duration": 3.0, "intensity": 0.5}),
            ),
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
        "freshCloseUp" => vec![
            floor,
            p("productTable", "table", [0.0, 0.0, 0.0], 0.0),
            p("cuttingBoard", "board", [0.0, 1.2, 0.0], 0.0),
            p("salmonFillet", "salmon", [-0.02, 1.2, 0.0], 12.0),
            p("lemon", "lemon", [0.13, 1.2, -0.08], 30.0),
            p("chefKnife", "knife", [0.05, 1.2, 0.1], -8.0),
            p("vineTomatoes", "tomatoes", [-0.33, 1.2, -0.17], 20.0),
            p("broccoli", "broccoli", [0.32, 1.2, -0.2], 0.0),
            p("plate", "plate", [-0.33, 1.2, 0.17], 0.0),
            p("leafBranch", "herbs", [0.3, 1.2, 0.18], -50.0),
            p("softbox", "softbox", [1.5, 0.0, 0.6], -110.0),
            p("reflector", "reflector", [-1.2, 0.0, 0.7], 120.0),
            ("light.set", json!({"id": "softbox_light", "intensity": 0.9})),
            timeline(167),
            cam("cam_top", "Top"),
            frame("cam_top", "board", "full", "front", "birdsEye", 50.0),
            motion("topDown", "cam_top", Some("board"), 0, 3.5),
            shot("cam_top", "from above", 0, 83),
            cam("cam_hero", "Hero"),
            frame("cam_hero", "salmon", "wide", "frontRight", "high", 100.0),
            (
                "motion.apply",
                json!({"preset": "pushIn", "target": "cam_hero", "subject": "salmon", "start": 84, "duration": 3.5, "intensity": 0.6}),
            ),
            shot("cam_hero", "hero push-in", 84, 167),
        ],
        "interview" => vec![
            floor,
            p("wall", "wall", [0.0, 0.0, -2.2], 0.0),
            p("chair", "chair_a", [-0.9, 0.0, 0.0], 90.0),
            p("chair", "chair_b", [0.9, 0.0, 0.0], -90.0),
            seated("humanMan", "host", [-0.9, 0.0, 0.0], 90.0)[0].clone(),
            seated("humanMan", "host", [-0.9, 0.0, 0.0], 90.0)[1].clone(),
            seated("humanWoman", "guest", [0.9, 0.0, 0.0], -90.0)[0].clone(),
            seated("humanWoman", "guest", [0.9, 0.0, 0.0], -90.0)[1].clone(),
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
            p("car", "car", [2.8, 0.0, -1.75], 90.0),
            p("tree", "tree", [-3.0, 0.0, -1.5], 0.0),
            p("tree", "tree_2", [5.0, 0.0, -1.5], 0.0),
            p("lamp", "street_lamp", [-1.5, 0.0, 0.5], 0.0),
            p("humanMan", "walker", [-3.0, 0.0, 0.0], 90.0),
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
        "checkout" => vec![
            floor,
            p("wall", "wall", [0.0, 0.0, -2.0], 0.0),
            p("checkout", "counter", [0.0, 0.0, 0.0], 0.0),
            p("humanCashier", "cashier", [0.15, 0.0, -0.62], 0.0),
            ("pose.set", json!({"id": "cashier", "pose": "scan"})),
            ("person.setExpression", json!({"id": "cashier", "expression": "smile", "amount": 0.7})),
            p("humanShopper", "shopper", [-0.5, 0.0, 0.72], 180.0),
            ("pose.set", json!({"id": "shopper", "pose": "pay"})),
            p("phone", "phone", [-0.5, 1.2, 0.4], 180.0),
            ("entity.attach", json!({"id": "phone", "to": "shopper", "hand": "right"})),
            p("jar", "jar", [0.55, 0.9, 0.05], 0.0),
            p("cup", "cup", [0.3, 0.9, -0.05], 0.0),
            p("pumpBottle", "bottle", [0.1, 0.9, 0.08], 0.0),
            p("basket", "basket", [0.2, 0.0, 0.85], 0.0),
            timeline(119),
            motion("talk", "cashier", None, 0, 3.0),
            cam("cam_a", "Two-shot"),
            frame("cam_a", "counter", "full", "right", "eyeLevel", 32.0),
            motion("handheld", "cam_a", Some("counter"), 0, 3.0),
            cam("cam_b", "Payment"),
            frame("cam_b", "phone", "wide", "left", "high", 50.0),
            ("light.preset", json!({"preset": "daylight", "camera": "cam_a"})),
            shot("cam_a", "two-shot", 0, 71),
            shot("cam_b", "payment", 72, 119),
        ],
        "cafe" => vec![
            floor,
            p("wall", "wall", [0.0, 0.0, -3.0], 0.0),
            p("table", "table", [0.0, 0.0, 0.0], 0.0),
            p("chair", "chair_a", [-0.75, 0.0, 0.0], 90.0),
            p("chair", "chair_b", [0.75, 0.0, 0.0], -90.0),
            seated("humanWoman", "friend_a", [-0.75, 0.0, 0.0], 90.0)[0].clone(),
            seated("humanWoman", "friend_a", [-0.75, 0.0, 0.0], 90.0)[1].clone(),
            seated("humanMan", "friend_b", [0.75, 0.0, 0.0], -90.0)[0].clone(),
            seated("humanMan", "friend_b", [0.75, 0.0, 0.0], -90.0)[1].clone(),
            p("cup", "cup_a", [-0.25, 1.0, 0.1], 0.0),
            p("cup", "cup_b", [0.25, 1.0, -0.1], 0.0),
            p("checkout", "counter", [2.6, 0.0, -1.8], -90.0),
            p("humanStaff", "barista", [3.3, 0.0, -1.8], -90.0),
            p("table", "table_2", [-2.4, 0.0, -1.2], 0.0),
            p("chair", "chair_c", [-2.4, 0.0, -0.4], 180.0),
            p("lamp", "lamp", [-3.2, 0.0, -2.4], 0.0),
            timeline(143),
            motion("talk", "friend_a", None, 0, 6.0),
            motion("laugh", "friend_b", None, 70, 2.5),
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

/// Builds a list of template steps.
#[derive(Default)]
struct Steps(Vec<(&'static str, Value)>);

impl Steps {
    fn add(&mut self, cmd: &'static str, params: Value) -> &mut Self {
        self.0.push((cmd, params));
        self
    }
    fn prefab(&mut self, kind: &str, id: &str, pos: [f32; 3], yaw: f32) -> &mut Self {
        self.add("prefab.create", json!({"kind": kind, "id": id, "position": pos, "rotationY": yaw}))
    }
    fn prefab_scaled(&mut self, kind: &str, id: &str, pos: [f32; 3], yaw: f32, scale: f32) -> &mut Self {
        self.add("prefab.create", json!({"kind": kind, "id": id, "position": pos, "rotationY": yaw, "scale": scale}))
    }
    /// Stretch a group (a plinth made lower, a riser made wider).
    fn stretch(&mut self, id: &str, scale: [f32; 3]) -> &mut Self {
        self.add("entity.setTransform", json!({"id": id, "scale": scale}))
    }
    /// Paint parts of a prefab: `group_part` for each part.
    fn paint(&mut self, group: &str, parts: &[&str], color: [f32; 3]) -> &mut Self {
        for part in parts {
            self.add("mesh.setColor", json!({"id": format!("{group}_{part}"), "color": color}));
        }
        self
    }
    /// A flat floor, or with `wall_z` also a backdrop wall standing at that depth.
    fn floor(&mut self, color: [f32; 3], wall: Option<(f32, [f32; 3])>) -> &mut Self {
        self.add(
            "entity.create",
            json!({"type": "mesh", "id": "floor", "name": "floor", "primitive": {"shape": "plane", "size": [8, 8]}, "color": color, "locked": true}),
        );
        if let Some((z, c)) = wall {
            self.add(
                "entity.create",
                json!({"type": "mesh", "id": "backdrop", "name": "backdrop", "primitive": {"shape": "plane", "size": [6, 4]}, "position": [0, 2, z], "rotationEuler": [90, 0, 0], "color": c, "locked": true}),
            );
        }
        self
    }
    fn cove(&mut self, color: [f32; 3]) -> &mut Self {
        self.prefab("infinityCove", "cove", [0.0, 0.0, 0.0], 0.0).paint("cove", &["sweep"], color)
    }
    /// The sky as one even colour all round (it lights softly and shows behind the set).
    fn sky(&mut self, color: [f32; 3], intensity: f32, background: bool) -> &mut Self {
        self.add(
            "environment.set",
            json!({"sky": "color", "color": color, "intensity": intensity, "background": background}),
        )
    }
    /// A light at `pos` aimed at `aim`; `light` is its kind, intensity, size, colour...
    fn light(&mut self, id: &str, pos: [f32; 3], aim: [f32; 3], light: Value) -> &mut Self {
        self.add("entity.create", json!({"type": "light", "id": id, "name": id, "position": pos, "light": light}))
            .add("camera.lookAt", json!({"id": id, "target": aim}))
    }
    fn shadows(&mut self, softness: f32, strength: f32) -> &mut Self {
        self.add("light.shadows", json!({"softness": softness, "strength": strength}))
    }
    fn timeline(&mut self, end: u32) -> &mut Self {
        self.add("timeline.set", json!({"fps": 24, "start": 0, "end": end}))
    }
    #[allow(clippy::too_many_arguments)]
    fn camera(
        &mut self,
        id: &str,
        name: &str,
        subject: &str,
        size: &str,
        side: &str,
        angle: &str,
        mm: f32,
    ) -> &mut Self {
        self.add("entity.create", json!({"type": "camera", "id": id, "name": name})).add(
            "camera.frame",
            json!({"camera": id, "subject": subject, "size": size, "side": side, "angle": angle, "focalLengthMm": mm, "makeActive": true}),
        )
    }
    fn motion(&mut self, preset: &str, cam: &str, subject: &str, start: u32, secs: f32, intensity: f32) -> &mut Self {
        self.add(
            "motion.apply",
            json!({"preset": preset, "target": cam, "subject": subject, "start": start, "duration": secs, "intensity": intensity}),
        )
    }
    fn shot(&mut self, cam: &str, name: &str, a: u32, b: u32) -> &mut Self {
        self.add("shot.create", json!({"camera": cam, "name": name, "start": a, "end": b}))
    }
}

/// Styled product close-ups: each light is placed by hand around the product (no stands in the
/// picture), with a sky and shadows to match.
fn product_look(name: &str) -> Option<Vec<(&'static str, Value)>> {
    let mut s = Steps::default();
    let area = |i: f32, w: f32, h: f32| json!({"kind": "area", "intensity": i, "size": [w, h]});
    let spot =
        |i: f32, cone: f32, soft: f32| json!({"kind": "spot", "intensity": i, "coneDegrees": cone, "softness": soft});
    let with = |mut l: Value, key: &str, v: Value| {
        l[key] = v;
        l
    };
    let no_shadow = |l: Value| with(l, "castShadows", json!(false));
    match name {
        "darkHero" => {
            let amber = [0.35, 0.22, 0.08];
            s.floor([0.03, 0.03, 0.035], Some((-1.6, [0.05, 0.05, 0.06])))
                .sky([0.0; 3], 0.0, true)
                .prefab("podium", "plinth", [0.0, 0.0, 0.0], 0.0)
                .paint("plinth", &["podium"], [0.04, 0.04, 0.045])
                .prefab("bottle", "hero", [0.0, 1.0, 0.0], 0.0)
                .paint("hero", &["body", "shoulder", "neck"], amber)
                .paint("hero", &["cap"], [0.75, 0.6, 0.3])
                .paint("hero", &["label"], [0.85, 0.82, 0.75])
                // strip lights behind on both sides draw the bottle's outline
                .light("rim_l", [-0.55, 0.55, -0.45], [0.0, 0.33, 0.0], area(0.9, 0.12, 0.9))
                .light("rim_r", [0.55, 0.55, -0.45], [0.0, 0.33, 0.0], area(0.9, 0.12, 0.9))
                .light("top", [0.0, 1.6, 0.35], [0.0, 0.2, 0.0], spot(0.7, 26.0, 0.7))
                .light("fill", [0.25, 0.35, 1.2], [0.0, 0.33, 0.0], no_shadow(area(0.12, 0.6, 0.6)))
                // a soft pool of light on the wall behind
                .light("halo", [0.0, 0.25, -0.2], [0.0, 0.75, -1.6], no_shadow(spot(0.5, 40.0, 1.0)))
                .timeline(143)
                .camera("cam_hero", "Hero", "hero", "wide", "front", "low", 100.0)
                .motion("orbitRight", "cam_hero", "hero", 0, 3.0, 0.4)
                .shot("cam_hero", "hero arc", 0, 71)
                .camera("cam_label", "Label", "hero", "full", "frontLeft", "eyeLevel", 135.0)
                .motion("pushIn", "cam_label", "hero", 72, 3.0, 0.4)
                .shot("cam_label", "label", 72, 143);
        }
        "highKeyWhite" => {
            let white = [0.92, 0.92, 0.92];
            let cream = [0.95, 0.95, 0.93];
            let sage = [0.45, 0.62, 0.5];
            s.cove(white)
                .sky([1.0; 3], 0.15, true)
                .light("cove_wash", [0.0, 1.2, 0.0], [0.0, 0.8, -1.9], no_shadow(area(0.5, 2.5, 1.0)))
                .prefab("cubeRiser", "riser_a", [-0.06, 0.0, 0.0], 0.0)
                .prefab("cubeRiser", "riser_b", [0.15, 0.0, 0.06], -12.0)
                .stretch("riser_b", [1.0, 0.5, 1.0])
                .paint("riser_a", &["riser"], white)
                .paint("riser_b", &["riser"], white)
                .prefab("pumpBottle", "hero", [-0.06, 1.0, 0.0], 0.0)
                .paint("hero", &["body"], sage)
                .paint("hero", &["collar", "stem", "head"], cream)
                .prefab("jar", "jar", [0.15, 1.0, 0.06], 0.0)
                .paint("jar", &["body"], [0.93, 0.93, 0.9])
                .paint("jar", &["lid"], sage)
                .prefab("tube", "tube", [-0.24, 0.0, 0.12], 10.0)
                .paint("tube", &["body", "crimp"], cream)
                .paint("tube", &["cap"], sage)
                .prefab("pebbles", "pebbles", [0.12, 0.0, 0.28], 0.0)
                // one big soft panel above and in front, two weak strips at the sides
                .light("key", [0.0, 1.4, 1.1], [0.0, 0.15, 0.0], area(0.55, 1.4, 1.0))
                .light("strip_l", [-0.9, 0.4, 0.2], [0.0, 0.2, 0.0], no_shadow(area(0.12, 0.3, 1.2)))
                .light("strip_r", [0.9, 0.4, 0.2], [0.0, 0.2, 0.0], no_shadow(area(0.12, 0.3, 1.2)))
                .shadows(0.9, 0.45)
                .timeline(143)
                .camera("cam_hero", "Hero", "riser_a", "wide", "front", "high", 70.0)
                .motion("truckRight", "cam_hero", "hero", 0, 3.0, 0.4)
                .shot("cam_hero", "set", 0, 71)
                .camera("cam_detail", "Detail", "hero", "full", "frontLeft", "high", 100.0)
                .motion("pushIn", "cam_detail", "hero", 72, 3.0, 0.4)
                .shot("cam_detail", "detail", 72, 143);
        }
        "goldenWindow" => {
            let dark = [0.2, 0.18, 0.16];
            s.cove([0.88, 0.84, 0.78])
                .sky([1.0, 0.93, 0.85], 0.25, false)
                .prefab("cubeRiser", "stone", [0.0, 0.0, 0.0], 0.0)
                .stretch("stone", [1.8, 1.0, 1.3])
                .paint("stone", &["riser"], [0.78, 0.72, 0.64])
                .prefab("pebbles", "pebbles", [0.2, 0.0, 0.25], 0.0)
                .prefab("pumpBottle", "hero", [-0.04, 1.0, 0.0], 0.0)
                .paint("hero", &["body"], [0.72, 0.45, 0.25])
                .paint("hero", &["collar", "stem", "head"], dark)
                .prefab("jar", "jar", [0.1, 1.0, 0.06], 0.0)
                .paint("jar", &["body"], [0.95, 0.92, 0.86])
                .paint("jar", &["lid"], [0.75, 0.6, 0.35])
                // a big leafy branch out of shot, between the sun and the set: its shadow is the
                // window-light pattern
                .add(
                    "prefab.create",
                    json!({"kind": "leafBranch", "id": "gobo", "name": "leaf shadow (gobo)", "position": [-1.2, 0.76, 0.6], "drop": false, "scale": 3.0}),
                )
                .add("entity.setTransform", json!({"id": "gobo", "rotationEuler": [0, 20, 80]}))
                .light("sun", [-3.0, 1.6, 1.5], [0.0, 0.2, 0.0], json!({"kind": "sun", "intensity": 1.8, "color": [1.0, 0.8, 0.6]}))
                .light("fill", [0.8, 0.4, 1.0], [0.0, 0.2, 0.0], with(no_shadow(area(0.25, 1.0, 1.0)), "color", json!([0.75, 0.82, 1.0])))
                .shadows(0.15, 0.8)
                .timeline(143)
                .camera("cam_hero", "Hero", "stone", "wide", "front", "high", 85.0)
                .motion("pushIn", "cam_hero", "hero", 0, 6.0, 0.4)
                .shot("cam_hero", "warm light", 0, 143);
        }
        "pastelPop" => {
            let pink = [0.95, 0.62, 0.68];
            let blue = [0.6, 0.78, 0.95];
            s.cove([0.97, 0.74, 0.76])
                .sky([1.0, 0.9, 0.92], 0.3, true)
                .prefab("podium", "base", [0.0, 0.0, 0.0], 0.0)
                .stretch("base", [0.9, 0.5, 0.9])
                .paint("base", &["podium"], [0.98, 0.86, 0.86])
                .prefab("podium", "top", [0.03, 1.0, -0.03], 0.0)
                .stretch("top", [0.45, 0.5, 0.45])
                .paint("top", &["podium"], blue)
                .prefab("can", "hero", [0.03, 1.0, -0.03], -20.0)
                .paint("hero", &["body"], [0.98, 0.45, 0.55])
                .paint("hero", &["top"], [0.85, 0.85, 0.88])
                .prefab("cubeRiser", "cube", [-0.28, 0.0, 0.05], 25.0)
                .stretch("cube", [0.45, 0.45, 0.45])
                .paint("cube", &["riser"], [1.0, 0.85, 0.5])
                .prefab_scaled("decorSpheres", "spheres", [0.26, 0.0, 0.12], 0.0, 0.7)
                .paint("spheres", &["large", "medium", "small"], blue)
                .prefab_scaled("decorSpheres", "spheres_2", [-0.2, 0.0, 0.25], 120.0, 0.45)
                .paint("spheres_2", &["large", "medium", "small"], pink)
                .light("key", [-0.8, 0.9, 0.9], [0.0, 0.2, 0.0], area(0.75, 0.9, 0.9))
                // coloured gels from behind on either side
                .light(
                    "gel_pink",
                    [0.8, 0.5, -0.5],
                    [0.0, 0.3, 0.0],
                    with(no_shadow(spot(0.7, 40.0, 0.8)), "color", json!([1.0, 0.45, 0.7])),
                )
                .light(
                    "gel_blue",
                    [-0.8, 0.5, -0.5],
                    [0.0, 0.3, 0.0],
                    with(no_shadow(spot(0.7, 40.0, 0.8)), "color", json!([0.4, 0.7, 1.0])),
                )
                .shadows(0.6, 0.5)
                .timeline(119)
                .camera("cam_hero", "Hero", "base", "wide", "front", "high", 65.0)
                .motion("orbitLeft", "cam_hero", "hero", 0, 5.0, 0.5)
                .shot("cam_hero", "pop", 0, 119);
        }
        "iceDrink" => {
            let cyan = json!([0.5, 0.85, 1.0]);
            s.floor([0.03, 0.06, 0.12], Some((-1.4, [0.04, 0.08, 0.16])))
                .sky([0.0, 0.02, 0.05], 0.2, true)
                .prefab("podium", "base", [0.0, 0.0, 0.0], 0.0)
                .stretch("base", [0.65, 0.4, 0.65])
                .paint("base", &["podium"], [0.12, 0.18, 0.3])
                .prefab("can", "hero", [0.0, 1.0, 0.0], 0.0)
                .paint("hero", &["body"], [0.15, 0.75, 0.6])
                .paint("hero", &["top"], [0.85, 0.87, 0.9])
                .prefab("iceCubes", "ice", [0.09, 1.0, 0.07], 0.0)
                .prefab("iceCubes", "ice_2", [-0.08, 1.0, 0.08], 60.0)
                .prefab("citrusSlice", "slice", [0.1, 1.0, -0.05], -20.0)
                .light("rim_l", [-0.6, 0.5, -0.5], [0.0, 0.25, 0.0], with(area(1.0, 0.15, 0.9), "color", cyan.clone()))
                .light("rim_r", [0.6, 0.5, -0.5], [0.0, 0.25, 0.0], with(area(1.0, 0.15, 0.9), "color", cyan))
                .light("key", [0.3, 0.9, 0.9], [0.0, 0.2, 0.0], area(0.45, 0.6, 0.6))
                .light(
                    "glow",
                    [0.0, 0.2, -0.3],
                    [0.0, 0.6, -1.4],
                    with(no_shadow(spot(0.6, 45.0, 1.0)), "color", json!([0.2, 0.55, 1.0])),
                )
                .timeline(119)
                .camera("cam_hero", "Hero", "hero", "wide", "front", "low", 85.0)
                .motion("pushIn", "cam_hero", "hero", 0, 5.0, 0.5)
                .shot("cam_hero", "hero", 0, 119);
        }
        "macroFruit" => {
            s.floor([0.05, 0.045, 0.04], None)
                .sky([0.02, 0.018, 0.015], 0.3, true)
                .prefab("cuttingBoard", "slate", [0.0, 0.0, 0.0], 0.0)
                .stretch("slate", [2.0, 1.0, 2.0])
                .paint("slate", &["board"], [0.08, 0.08, 0.085])
                .prefab("strawberry", "berry", [0.0, 1.0, 0.03], 10.0)
                .prefab("strawberry", "berry_2", [0.045, 1.0, -0.02], 70.0)
                .prefab("strawberry", "berry_3", [-0.04, 1.0, -0.03], -30.0)
                .prefab("grapes", "grapes", [-0.1, 1.0, -0.1], 40.0)
                .prefab("lemon", "lemon", [0.16, 1.0, -0.14], -20.0)
                .prefab("citrusSlice", "slice", [0.1, 1.0, -0.07], 0.0)
                // warm light from behind makes the fruit glow at the edges; soft side key
                .light("back", [0.0, 0.35, -0.7], [0.0, 0.03, 0.0], with(spot(1.4, 35.0, 0.7), "color", json!([1.0, 0.86, 0.7])))
                .light("key", [-0.6, 0.45, 0.25], [0.0, 0.03, 0.0], area(0.55, 0.5, 0.5))
                .light("fill", [0.5, 0.2, 0.5], [0.0, 0.03, 0.0], no_shadow(area(0.12, 0.5, 0.5)))
                .shadows(0.5, 0.7)
                .timeline(119)
                .add(
                    "entity.create",
                    json!({"type": "camera", "id": "cam_macro", "name": "Macro", "position": [0.0, 0.2, 0.75], "camera": {"focalLengthMm": 100}, "makeActive": true}),
                )
                .add("camera.lookAt", json!({"id": "cam_macro", "target": [0.0, 0.035, 0.0]}))
                .motion("truckLeft", "cam_macro", "berry", 0, 5.0, 0.15)
                .shot("cam_macro", "macro slide", 0, 119);
        }
        _ => return None,
    }
    Some(s.0)
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
        command!("scene.template", "Start from a ready scene (scene.templates lists them): sets like groceryAisle, interview, streetCorner, cafe or studio, and styled product close-ups with tuned lights: productTabletop, packshot, darkHero, highKeyWhite, goldenWindow, pastelPop, iceDrink, macroFruit, freshCloseUp (people, props, lights, cameras, shots and moves; all editable). Clears the scene first unless replace is false.", mutates: true, TemplateParams, apply),
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
