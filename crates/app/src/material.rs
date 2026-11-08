//! The "Texture" section of a mesh's properties: pick a picture, choose how it is laid on
//! (box / planar / cylinder / sphere projection or the mesh's UV map), repeat / offset / turn
//! it, and make a UV map (`mesh.setMaterial`, `mesh.unwrap`, `mesh.uvTransform`).

use shotstage_core::uv::{Mapping, Material};

use super::*;

const MAPPINGS: [(Mapping, &str, &str); 5] = [
    (Mapping::Box, "box", "Box: each face takes the picture from the side it faces most"),
    (Mapping::Planar, "planar", "Planar: projected straight along one axis"),
    (Mapping::Cylinder, "cylinder", "Cylinder: wrapped around an axis, like a can label"),
    (Mapping::Sphere, "sphere", "Sphere: wrapped like a globe"),
    (Mapping::Uv, "uv", "UV map: follows the mesh's own UVs (from Unwrap or an imported model)"),
];

fn mapping_name(m: Mapping) -> &'static str {
    MAPPINGS.iter().find(|(x, ..)| *x == m).map(|(_, n, _)| *n).unwrap_or("box")
}

/// How a mapping is shown in the panel.
pub(super) fn mapping_label(name: &str) -> &str {
    match name {
        _ if !crate::i18n::zh() => match name {
            "box" => "Box",
            "planar" => "Planar",
            "cylinder" => "Cylinder",
            "sphere" => "Sphere",
            _ => "UV",
        },
        "box" => "盒状",
        "planar" => "平面",
        "cylinder" => "圆柱",
        "sphere" => "球面",
        _ => "UV",
    }
}

impl EditorApp {
    pub(super) fn material_panel(&mut self, ui: &mut egui::Ui, id: &str, material: Option<&Material>) {
        let scene = self.scene();
        let mat = material.cloned().unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Picture"));
            let current = mat.texture.as_deref().and_then(|k| scene.assets.get(k).map(|a| (k, a)));
            let label = match current {
                Some((k, a)) => format!("{} ({}×{})", k, a.width, a.height),
                None => tr("none"),
            };
            egui::ComboBox::from_id_salt(("texture", id)).selected_text(label).show_ui(ui, |ui| {
                for (k, a) in &scene.assets {
                    let on = mat.texture.as_deref() == Some(k.as_str());
                    if ui.selectable_label(on, format!("{} ({}×{})", k, a.width, a.height)).clicked() && !on {
                        self.run("mesh.setMaterial", json!({"id": id, "texture": k}));
                    }
                }
                if scene.assets.is_empty() {
                    ui.weak(tr("No pictures in the scene yet"));
                }
            });
            if ui.button(tr("Load…")).on_hover_text(tr("Use a PNG, JPEG or WebP file as this mesh's texture")).clicked()
            {
                self.texture_for = Some(id.to_string());
                crate::files::pick(&self.incoming, ui.ctx());
            }
            if mat.texture.is_some() && ui.small_button(tr("✕")).on_hover_text(tr("Remove the texture")).clicked() {
                self.run("mesh.setMaterial", json!({"id": id, "clear": true}));
            }
        });
        if mat.texture.is_none() {
            ui.weak(tr("Give the mesh a picture: a label, wood grain, a packaging print…"));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Laid on as"));
            for (m, name, tip) in MAPPINGS {
                if ui.selectable_label(mat.mapping == m, mapping_label(name)).on_hover_text(tr(tip)).clicked()
                    && mat.mapping != m
                {
                    self.run("mesh.setMaterial", json!({"id": id, "mapping": mapping_name(m)}));
                }
            }
        });
        if matches!(mat.mapping, Mapping::Planar | Mapping::Cylinder) {
            let axis = mat.axis.unwrap_or(if mat.mapping == Mapping::Planar { 2 } else { 1 });
            ui.horizontal(|ui| {
                ui.label(tr("Axis"));
                for (i, n) in ["X", "Y", "Z"].iter().enumerate() {
                    if ui.selectable_label(axis as usize == i, tr(*n)).clicked() && axis as usize != i {
                        self.run("mesh.setMaterial", json!({"id": id, "axis": i}));
                    }
                }
            });
        }
        let (mut rep, mut off, mut rot) = (mat.repeat, mat.offset, mat.rotation);
        let mut changed = false;
        let mut started = false;
        egui::Grid::new(("texture-grid", id)).num_columns(3).show(ui, |ui| {
            for (label, v, speed, min) in [("Repeat", &mut rep, 0.02, 0.01), ("Offset", &mut off, 0.01, -1e6)] {
                ui.label(tr(label));
                for x in v.iter_mut() {
                    let r = ui
                        .add_sized([56.0, 20.0], egui::DragValue::new(x).speed(speed).range(min..=1e6).max_decimals(3));
                    changed |= r.changed();
                    started |= r.drag_started() || r.gained_focus();
                }
                ui.end_row();
            }
            ui.label(tr("Turn"));
            let r = ui.add_sized([56.0, 20.0], egui::DragValue::new(&mut rot).speed(1.0).suffix("°"));
            changed |= r.changed();
            started |= r.drag_started() || r.gained_focus();
            ui.end_row();
        });
        if started {
            self.gesture += 1;
        }
        if changed {
            self.run_merged(
                &format!("material:{id}"),
                "mesh.setMaterial",
                json!({"id": id, "repeat": rep, "offset": off, "rotation": rot}),
            );
        }

        // UV map
        let picked = self.edit_faces(id).filter(|f| !f.is_empty());
        let scope = tr(if picked.is_some() { "selected faces" } else { "whole mesh" });
        ui.horizontal_wrapped(|ui| {
            let open = self.uv_is_open(id);
            if ui
                .selectable_label(open, tr("UV editor"))
                .on_hover_text(tr("Open the UV map beside the view: see it over the picture and drag its vertices, edges, faces and islands"))
                .clicked()
            {
                self.uv_toggle(id);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::lf!("Unwrap {scope}", "展开 UV（{scope}）"));
            for (method, tip) in [
                ("smart", "Cut into flat pieces and pack them without overlap"),
                ("box", "Bake a box projection into the UV map"),
                ("cylinder", "Bake a cylinder projection (can, bottle)"),
                ("sphere", "Bake a sphere projection"),
            ] {
                let label = match method {
                    "smart" => tr("Smart"),
                    m => mapping_label(m).to_string(),
                };
                if ui.button(tr(label)).on_hover_text(tr(tip)).clicked() {
                    let mut p = json!({"id": id, "method": method});
                    if let Some(f) = &picked {
                        p["faces"] = json!(f);
                    }
                    self.run("mesh.unwrap", p);
                }
            }
        });
        if mat.mapping == Mapping::Uv {
            ui.horizontal_wrapped(|ui| {
                ui.label(crate::lf!("UVs of {scope}", "UV（{scope}）"));
                for (label, tip, p) in [
                    ("⟲ 90°", "Turn the picture on these faces", json!({"rotate": -90})),
                    ("Tile ×2", "Show twice as much of the picture", json!({"scale": [2, 2]})),
                    ("Zoom ×2", "Zoom into the picture", json!({"scale": [0.5, 0.5]})),
                    ("Mirror", "Mirror the picture left-right", json!({"scale": [-1, 1]})),
                ] {
                    if ui.small_button(tr(label)).on_hover_text(tr(tip)).clicked() {
                        let mut p = p;
                        p["id"] = json!(id);
                        if let Some(f) = &picked {
                            p["faces"] = json!(f);
                        }
                        self.run("mesh.uvTransform", p);
                    }
                }
            });
            if picked.is_none() {
                ui.weak(tr("Tip: in edit mode, pick faces to unwrap or nudge just those."));
            }
        }
    }
}
