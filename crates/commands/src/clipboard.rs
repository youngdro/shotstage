//! Copy and paste: whole objects (their parts, animation and pictures) as one JSON value, so
//! they travel between scenes, browser tabs, files and agents.

use std::collections::{BTreeMap, HashMap};

use glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shotstage_core::spatial;
use shotstage_core::{Asset, Entity, EntityKind, KeyValue, MeshSource, Scene, Track, TrackProperty};

use crate::{CmdResult, Command, CommandError, command};

/// Copied objects. `shotstageClip` marks the JSON (it is 1).
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clip {
    pub shotstage_clip: u32,
    /// The copied entities with all their descendants, parents first.
    pub entities: Vec<Entity>,
    /// Their animation.
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Pictures their image cards use, by key.
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CopyParams {
    /// Entities to copy (each with its children).
    ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PasteParams {
    /// What `entity.copy` returned.
    clip: Clip,
    /// Move the pasted objects by this much, metres (default: where they were).
    #[serde(default)]
    offset: Option<[f32; 3]>,
}

/// Point the references inside `e` (parent, skin joints, picture) at their new ids / keys.
pub(crate) fn remap_refs(e: &mut Entity, ids: &HashMap<String, String>, assets: &HashMap<String, String>) {
    if let Some(p) = &e.parent {
        e.parent = ids.get(p).cloned().or(Some(p.clone()));
    }
    match &mut e.kind {
        EntityKind::Mesh { source: MeshSource::Skinned { skin, .. }, .. } => {
            for j in &mut skin.joints {
                if let Some(n) = ids.get(j) {
                    *j = n.clone();
                }
            }
        }
        EntityKind::Mesh { source: MeshSource::Human { joints, .. }, .. } => {
            for j in joints {
                if let Some(n) = ids.get(j) {
                    *j = n.clone();
                }
            }
        }
        EntityKind::Image(img) => {
            if let Some(k) = assets.get(&img.asset) {
                img.asset = k.clone();
            }
        }
        _ => {}
    }
}

/// New ids for `subtree` (root first) pasted into `scene`: the root gets a free id and
/// `<root>_<part>` becomes `<newRoot>_<part>`, so groups and people stay readable and posable.
pub(crate) fn fresh_ids(scene: &Scene, subtree: &[String], taken: &mut Vec<String>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(root) = subtree.first() else { return map };
    let free = |id: &str, taken: &[String]| !scene.contains(id) && !taken.iter().any(|t| t == id);
    let unique = |stem: &str, taken: &[String]| {
        if free(stem, taken) {
            stem.to_string()
        } else {
            (1..).map(|n| format!("{stem}_{n}")).find(|c| free(c, taken)).expect("unbounded")
        }
    };
    let new_root = unique(root, taken);
    taken.push(new_root.clone());
    map.insert(root.clone(), new_root.clone());
    for id in &subtree[1..] {
        let stem =
            id.strip_prefix(&format!("{root}_")).map(|rest| format!("{new_root}_{rest}")).unwrap_or_else(|| id.clone());
        let fresh = unique(&stem, taken);
        taken.push(fresh.clone());
        map.insert(id.clone(), fresh);
    }
    map
}

fn copy(s: &mut Scene, p: CopyParams) -> CmdResult<Value> {
    for id in &p.ids {
        s.get(id)?;
    }
    // skip ids already inside another copied one
    let roots: Vec<&String> =
        p.ids.iter().filter(|i| !p.ids.iter().any(|o| o != *i && spatial::subtree(s, o).contains(i))).collect();
    let mut entities = Vec::new();
    for r in roots {
        for id in spatial::subtree(s, r) {
            let mut e = s.get(&id)?.clone();
            if id == *r {
                // a copied child comes out where it is in the world, at the top level
                if e.parent.is_some() {
                    let w = shotstage_core::eval::world_at(s, &id, 0.0)?;
                    let (sc, r, t) = w.to_scale_rotation_translation();
                    e.transform = shotstage_core::Transform::from_parts(t, r, sc);
                }
                e.parent = None;
            }
            entities.push(e);
        }
    }
    let ids: Vec<&str> = entities.iter().map(|e| e.id.as_str()).collect();
    let tracks: Vec<Track> = s.timeline.tracks.iter().filter(|t| ids.contains(&t.entity.as_str())).cloned().collect();
    let assets = entities
        .iter()
        .filter_map(|e| match &e.kind {
            EntityKind::Image(img) => s.assets.get(&img.asset).map(|a| (img.asset.clone(), a.clone())),
            _ => None,
        })
        .collect();
    let clip = Clip { shotstage_clip: 1, entities, tracks, assets };
    Ok(serde_json::to_value(clip).unwrap_or(Value::Null))
}

fn paste(s: &mut Scene, p: PasteParams) -> CmdResult<Value> {
    let clip = p.clip;
    if clip.shotstage_clip != 1 {
        return Err(CommandError::invalid("not a shotstage clip (shotstageClip must be 1)"));
    }
    if clip.entities.is_empty() || clip.entities.len() > 20_000 {
        return Err(CommandError::invalid("a clip holds 1 to 20000 entities"));
    }
    let off = Vec3::from_array(p.offset.unwrap_or([0.0; 3]));
    if !off.is_finite() {
        return Err(CommandError::invalid("`offset` must be numbers"));
    }
    // pictures: reuse an identical one, otherwise add under a free key
    let mut asset_keys = HashMap::new();
    for (k, a) in &clip.assets {
        let key = match s.assets.get(k) {
            Some(have) if have == a => k.clone(),
            Some(_) => (1..).map(|n| format!("{k}_{n}")).find(|c| !s.assets.contains_key(c)).expect("unbounded"),
            None => k.clone(),
        };
        s.assets.entry(key.clone()).or_insert_with(|| a.clone());
        asset_keys.insert(k.clone(), key);
    }
    // one subtree per top-level entity of the clip
    let in_clip: Vec<String> = clip.entities.iter().map(|e| e.id.clone()).collect();
    let in_clip: Vec<&str> = in_clip.iter().map(String::as_str).collect();
    let mut ids = HashMap::new();
    let mut taken = Vec::new();
    let mut roots = Vec::new();
    for r in clip.entities.iter().filter(|e| e.parent.as_deref().is_none_or(|p| !in_clip.contains(&p))) {
        // the clip's own hierarchy under r, parents first
        let mut sub = vec![r.id.clone()];
        let mut i = 0;
        while i < sub.len() {
            let cur = sub[i].clone();
            sub.extend(
                clip.entities.iter().filter(|e| e.parent.as_deref() == Some(cur.as_str())).map(|e| e.id.clone()),
            );
            i += 1;
        }
        ids.extend(fresh_ids(s, &sub, &mut taken));
        roots.push(r.id.clone());
    }
    // parents before children whatever the clip's order
    let mut pending: Vec<Entity> = clip.entities;
    let mut done: Vec<String> = Vec::new();
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = Vec::new();
        for mut e in pending {
            let ready = e.parent.as_deref().is_none_or(|p| !in_clip.contains(&p) || done.iter().any(|d| d == p));
            if !ready {
                rest.push(e);
                continue;
            }
            let old = e.id.clone();
            let Some(new) = ids.get(&old).cloned() else { continue };
            if roots.contains(&old) {
                e.parent = None;
                e.transform.translation = (e.transform.translation() + off).to_array();
            }
            remap_refs(&mut e, &ids, &asset_keys);
            e.id = new;
            s.insert(e)?;
            done.push(old);
        }
        if rest.len() == before {
            return Err(CommandError::invalid("the clip's parents form a loop"));
        }
        pending = rest;
    }
    for mut t in clip.tracks {
        let Some(new) = ids.get(&t.entity).cloned() else { continue };
        if roots.contains(&t.entity) && t.property == TrackProperty::Transform && off != Vec3::ZERO {
            for k in &mut t.keys {
                if let KeyValue::Transform(tr) = &mut k.value {
                    tr.translation = (tr.translation() + off).to_array();
                }
            }
        }
        t.entity = new;
        s.timeline.tracks.retain(|o| !(o.entity == t.entity && o.property == t.property));
        s.timeline.tracks.push(t);
    }
    let new_roots: Vec<&String> = roots.iter().filter_map(|r| ids.get(r)).collect();
    Ok(json!({ "ids": new_roots, "entities": done.len() }))
}

pub fn commands() -> Vec<Command> {
    vec![
        command!("entity.copy", "Copy entities (with their children, animation and pictures) as a clip: JSON that entity.paste puts into this or any other scene.", mutates: false, CopyParams,
            |s, p| copy(s, p)),
        command!("entity.paste", "Paste a clip from entity.copy. Ids that are taken get fresh ones (a person's joints follow its new id); `offset` moves the pasted objects.", mutates: true, PasteParams,
            |s, p| paste(s, p)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn copy_paste_between_scenes_keeps_people_posable() {
        let mut a = crate::Session::default();
        a.dispatch("prefab.create", json!({"kind": "woman", "id": "w"})).unwrap();
        a.dispatch("motion.apply", json!({"preset": "wave", "target": "w", "start": 0})).unwrap();
        let clip = a.dispatch("entity.copy", json!({"ids": ["w"]})).unwrap().result;
        let n = clip["entities"].as_array().unwrap().len();
        assert!(n > 10);

        // into the same scene: fresh ids, still a person, animation along
        let r = a.dispatch("entity.paste", json!({"clip": clip, "offset": [1, 0, 0]})).unwrap().result;
        let id = r["ids"][0].as_str().unwrap().to_string();
        assert_ne!(id, "w");
        assert_eq!(r["entities"], n);
        assert_eq!(crate::rig::person_root(a.scene(), &format!("{id}_elbow_r")).unwrap(), id);
        assert!(a.scene().timeline.tracks.iter().any(|t| t.entity == format!("{id}_shoulder_r")));
        a.dispatch("pose.set", json!({"id": id, "pose": "sit"})).unwrap();
        let x0 = shotstage_core::eval::world_at(a.scene(), "w", 0.0).unwrap().w_axis.x;
        let x1 = shotstage_core::eval::world_at(a.scene(), &id, 0.0).unwrap().w_axis.x;
        assert!((x1 - x0 - 1.0).abs() < 1e-4);
        // one undo step
        a.dispatch("history.undo", json!({})).unwrap();
        a.dispatch("history.undo", json!({})).unwrap();
        assert!(!a.scene().contains(&id));

        // into an empty scene: same ids
        let mut b = crate::Session::default();
        let r = b.dispatch("entity.paste", json!({"clip": clip})).unwrap().result;
        assert_eq!(r["ids"][0], "w");
        assert_eq!(b.scene().entities.len(), n);
    }

    #[test]
    fn duplicate_and_paste_rebind_skins() {
        let mut s = crate::Session::default();
        s.dispatch("entity.create", json!({"type": "empty", "id": "rig"})).unwrap();
        s.dispatch("entity.create", json!({"type": "empty", "id": "rig_bone", "parent": "rig"})).unwrap();
        let mut e = s.scene().get("rig").unwrap().clone();
        e.id = "rig_body".into();
        e.parent = Some("rig".into());
        e.kind = EntityKind::Mesh {
            source: MeshSource::Skinned {
                mesh: shotstage_core::Primitive::Cube { size: [1.0; 3] }.build().unwrap().into(),
                skin: shotstage_core::Skin {
                    joints: vec!["rig_bone".into()],
                    inverse_bind: vec![glam::Mat4::IDENTITY.to_cols_array()],
                    vertex_joints: vec![[0; 4]; 24],
                    vertex_weights: vec![[1.0, 0.0, 0.0, 0.0]; 24],
                }
                .into(),
            },
            modifiers: vec![],
            color: [0.5; 3],
            material: None,
        };
        let mut scene = s.scene().clone();
        scene.insert(e).unwrap();
        s.replace_scene(scene);
        let joints = |s: &crate::Session, id: &str| match &s.scene().get(id).unwrap().kind {
            EntityKind::Mesh { source: MeshSource::Skinned { skin, .. }, .. } => skin.joints.clone(),
            _ => vec![],
        };
        let d = s.dispatch("entity.duplicate", json!({"id": "rig"})).unwrap().result;
        let copy = d["id"].as_str().unwrap();
        assert_eq!(joints(&s, &format!("{copy}_body")), vec![format!("{copy}_bone")]);
        let clip = s.dispatch("entity.copy", json!({"ids": ["rig"]})).unwrap().result;
        let p = s.dispatch("entity.paste", json!({"clip": clip})).unwrap().result;
        let pasted = p["ids"][0].as_str().unwrap();
        assert_eq!(joints(&s, &format!("{pasted}_body")), vec![format!("{pasted}_bone")]);
    }
}
