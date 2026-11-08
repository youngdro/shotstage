//! Stylized rigged people from bundled GLB files, made from Quaternius' Universal Base
//! Characters (CC0, see assets/LICENSES.md). (Realistic people are the MakeHuman humans of
//! `humans.rs`.) Their skeletons use shotstage's joint names with identity rest
//! rotations (tools/people/), so poses, motion presets, joint dragging, hold in hand and look
//! at camera work on them as on the built-in man and woman.
//!
//! The desktop editor, CLI and MCP server carry the files; on the web the page loads
//! `people/<file>` over HTTP and passes the bytes as `data`, so the wasm stays small.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use shotstage_core::Scene;

use crate::{CmdResult, Command, CommandError, command};

pub struct PersonAsset {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// File name under assets/people (and `people/` next to the web page).
    pub file: &'static str,
}

pub const PEOPLE: &[PersonAsset] = &[
    PersonAsset {
        key: "stylizedMan",
        label: "Stylized man",
        description: "Stylized athletic man, side-parted hair (Quaternius, CC0); full skeleton with fingers",
        file: "stylized_man.glb",
    },
    PersonAsset {
        key: "stylizedManBeard",
        label: "Bearded man",
        description: "Stylized athletic man, buzz cut and beard (Quaternius, CC0); full skeleton with fingers",
        file: "stylized_man_beard.glb",
    },
    PersonAsset {
        key: "stylizedWoman",
        label: "Stylized woman",
        description: "Stylized athletic woman, long hair (Quaternius, CC0); full skeleton with fingers",
        file: "stylized_woman.glb",
    },
    PersonAsset {
        key: "stylizedWomanBuns",
        label: "Woman with buns",
        description: "Stylized athletic woman, hair in buns (Quaternius, CC0); full skeleton with fingers",
        file: "stylized_woman_buns.glb",
    },
];

/// The bundled file of a person, where this build carries them (not on the web).
#[cfg(not(target_arch = "wasm32"))]
pub fn bundled(key: &str) -> Option<&'static [u8]> {
    macro_rules! files {
        ($($k:literal => $f:literal),* $(,)?) => {
            match key { $($k => Some(include_bytes!(concat!("../../../assets/people/", $f)) as &[u8]),)* _ => None }
        };
    }
    files!(
        "stylizedMan" => "stylized_man.glb",
        "stylizedManBeard" => "stylized_man_beard.glb",
        "stylizedWoman" => "stylized_woman.glb",
        "stylizedWomanBuns" => "stylized_woman_buns.glb",
    )
}

#[cfg(target_arch = "wasm32")]
pub fn bundled(_key: &str) -> Option<&'static [u8]> {
    None
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateParams {
    /// Which person: see `person.catalog`.
    variant: String,
    /// The GLB file, base64 (needed on the web, where the page loads `people/<file>`).
    data: Option<String>,
    id: Option<String>,
    /// Where the feet go, metres. Default `[0, 0, 0]`.
    position: Option<[f32; 3]>,
    /// Turn about the vertical axis, degrees (0 faces +Z).
    rotation_y: Option<f32>,
    /// Drop onto whatever is below. Default true.
    drop: Option<bool>,
}

fn create(s: &mut Scene, p: CreateParams) -> CmdResult<Value> {
    let asset = PEOPLE.iter().find(|a| a.key == p.variant).ok_or_else(|| {
        CommandError::invalid(format!(
            "no person `{}`; use one of {}",
            p.variant,
            PEOPLE.iter().map(|a| a.key).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let data = match (p.data, bundled(asset.key)) {
        (Some(d), _) => d,
        (None, Some(bytes)) => crate::model::encode(bytes),
        (None, None) => {
            return Err(CommandError::invalid(format!(
                "this build does not carry `{}`: load people/{} and pass it as `data` (base64)",
                asset.key, asset.file
            )));
        }
    };
    let stem: String = asset.label.to_lowercase().replace(' ', "_");
    let mut params = json!({
        "format": "glb", "data": data, "name": asset.label,
        "position": p.position.unwrap_or([0.0; 3]), "rotationY": p.rotation_y.unwrap_or(0.0),
        "drop": p.drop.unwrap_or(true),
    });
    params["id"] = json!(p.id.unwrap_or_else(|| s.unique_id(&stem)));
    let set = crate::registry().into_iter().find(|c| c.id == "model.import").expect("registered");
    let r = (set.run)(s, params)?;
    let id = r["id"].as_str().unwrap_or_default().to_string();
    crate::rig::person_root(s, &id)
        .map_err(|_| CommandError::new("failed", format!("{} has no shotstage skeleton", asset.file)))?;
    Ok(json!({ "id": id, "variant": asset.key, "size": r["size"] }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("person.create", "Add a stylized rigged person (see person.catalog; realistic people are prefab.create human*). Poses, motion presets, hold in hand and look at camera work on them.", mutates: true, CreateParams, create),
        command!("person.catalog", "The stylized people person.create can add: key, label, description and file.", mutates: false, crate::builtin::NoParams,
            |_, _| Ok(json!(PEOPLE.iter().map(|a| json!({
                "variant": a.key, "label": a.label,
                "description": a.description, "file": format!("people/{}", a.file), "bundled": bundled(a.key).is_some(),
            })).collect::<Vec<_>>()))),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn every_bundled_person_poses_and_animates() {
        for a in super::PEOPLE {
            let mut s = crate::Session::default();
            let r = s
                .dispatch("person.create", json!({"variant": a.key, "id": "p"}))
                .unwrap_or_else(|e| panic!("{}: {e}", a.key));
            let size = r.result["size"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
            assert!(size[1] > 1.4 && size[1] < 2.1, "{}: height {}", a.key, size[1]);
            assert!(size[0] < 0.75, "{}: arms down, width {}", a.key, size[0]);
            for pose in ["sit", "wave", "pickUp"] {
                s.dispatch("pose.set", json!({"id": "p", "pose": pose}))
                    .unwrap_or_else(|e| panic!("{} {pose}: {e}", a.key));
            }
            for preset in ["walkCycle", "wave", "sitDown", "pickUp", "turnHead"] {
                s.dispatch("motion.apply", json!({"preset": preset, "target": "p", "start": 0}))
                    .unwrap_or_else(|e| panic!("{} {preset}: {e}", a.key));
            }
            s.dispatch("person.lookAt", json!({"id": "p", "target": [0, 1.6, 3]})).unwrap();
            s.dispatch("prefab.create", json!({"kind": "phone", "id": "ph"})).unwrap();
            s.dispatch("entity.attach", json!({"id": "ph", "to": "p", "hand": "right"})).unwrap();
        }
    }
}
