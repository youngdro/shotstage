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

impl EditorApp {
    pub(super) fn material_panel(&mut self, ui: &mut egui::Ui, id: &str, material: Option<&Material>) {
        let scene = self.scene();
        let mat = material.cloned().unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label("Picture");
            let current = mat.texture.as_deref().and_then(|k| scene.assets.get(k).map(|a| (k, a)));
            let label = match current {
                Some((k, a)) => format!("{} ({}×{})", k, a.width, a.height),
                None => "none".into(),
            };
            egui::ComboBox::from_id_salt(("texture", id)).selected_text(label).show_ui(ui, |ui| {
                for (k, a) in &scene.assets {
                    let on = mat.texture.as_deref() == Some(k.as_str());
                    if ui.selectable_label(on, format!("{} ({}×{})", k, a.width, a.height)).clicked() && !on {
                        self.run("mesh.setMaterial", json!({"id": id, "texture": k}));
                    }
                }
                if scene.assets.is_empty() {
                    ui.weak("No pictures in the scene yet");
                }
            });
            if ui.button("Load…").on_hover_text("Use a PNG, JPEG or WebP file as this mesh's texture").clicked() {
                self.texture_for = Some(id.to_string());
                crate::files::pick(&self.incoming, ui.ctx());
            }
            if mat.texture.is_some() && ui.small_button("✕").on_hover_text("Remove the texture").clicked() {
                self.run("mesh.setMaterial", json!({"id": id, "clear": true}));
            }
        });
        if mat.texture.is_none() {
            ui.weak("Give the mesh a picture: a label, wood grain, a packaging print…");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Laid on as");
            for (m, name, tip) in MAPPINGS {
                if ui.selectable_label(mat.mapping == m, name).on_hover_text(tip).clicked() && mat.mapping != m {
                    self.run("mesh.setMaterial", json!({"id": id, "mapping": mapping_name(m)}));
                }
            }
        });
        if matches!(mat.mapping, Mapping::Planar | Mapping::Cylinder) {
            let axis = mat.axis.unwrap_or(if mat.mapping == Mapping::Planar { 2 } else { 1 });
            ui.horizontal(|ui| {
                ui.label("Axis");
                for (i, n) in ["X", "Y", "Z"].iter().enumerate() {
                    if ui.selectable_label(axis as usize == i, *n).clicked() && axis as usize != i {
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
                ui.label(label);
                for x in v.iter_mut() {
                    let r = ui
                        .add_sized([56.0, 20.0], egui::DragValue::new(x).speed(speed).range(min..=1e6).max_decimals(3));
                    changed |= r.changed();
                    started |= r.drag_started() || r.gained_focus();
                }
                ui.end_row();
            }
            ui.label("Turn");
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
        let scope = if picked.is_some() { "selected faces" } else { "whole mesh" };
        ui.horizontal_wrapped(|ui| {
            let open = self.uv_is_open(id);
            if ui
                .selectable_label(open, "UV editor")
                .on_hover_text("Open the UV map beside the view: see it over the picture and drag its vertices, edges, faces and islands")
                .clicked()
            {
                self.uv_toggle(id);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Unwrap {scope}"));
            for (method, label, tip) in [
                ("smart", "Smart", "Cut into flat pieces and pack them without overlap"),
                ("box", "Box", "Bake a box projection into the UV map"),
                ("cylinder", "Cylinder", "Bake a cylinder projection (can, bottle)"),
                ("sphere", "Sphere", "Bake a sphere projection"),
            ] {
                if ui.button(label).on_hover_text(tip).clicked() {
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
                ui.label(format!("UVs of {scope}"));
                for (label, tip, p) in [
                    ("⟲ 90°", "Turn the picture on these faces", json!({"rotate": -90})),
                    ("Tile ×2", "Show twice as much of the picture", json!({"scale": [2, 2]})),
                    ("Zoom ×2", "Zoom into the picture", json!({"scale": [0.5, 0.5]})),
                    ("Mirror", "Mirror the picture left-right", json!({"scale": [-1, 1]})),
                ] {
                    if ui.small_button(label).on_hover_text(tip).clicked() {
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
                ui.weak("Tip: in edit mode, pick faces to unwrap or nudge just those.");
            }
        }
    }
}
