//! The curve editor under the timeline: the animated numbers of one object (position,
//! rotation, scale, focal length) drawn as curves over time. Drag a key up or down to change
//! its value, sideways to retime it; click the stretch between two keys to shape how it eases
//! with two Bezier handles. Every change is a command (`keyframe.setChannel`, `keyframe.move`,
//! `keyframe.setInterp`, `keyframe.set`), one undo step per drag.

use std::collections::HashSet;

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Vec2};
use serde_json::{Value, json};
use shotstage_core::timeline::unwrap_degrees;
use shotstage_core::{Channel, Interp, Scene, Track, TrackProperty};

use crate::i18n::tr;

/// What the editor asks the app to do.
pub enum CurveAction {
    /// Run a command; repeated runs with the same merge key share one undo step.
    Run { merge: Option<String>, command: &'static str, params: Value },
    /// Put the playhead here.
    SetFrame(f32),
}

/// A key being dragged: which, from where, and the value range frozen at the start so the
/// curve does not rescale under the pointer.
#[derive(Clone)]
struct KeyDrag {
    property: TrackProperty,
    channel: Channel,
    /// The key's frame now (it changes while retiming).
    frame: f32,
    from_frame: f32,
    from_value: f32,
    origin: Pos2,
    /// Decided after a few pixels: true = sideways (retime), false = up/down (value).
    sideways: Option<bool>,
    range: (f32, f32),
    merge: String,
}

#[derive(Default)]
pub struct CurveEditor {
    /// Channels switched off in the list.
    hidden: HashSet<Channel>,
    /// Show the channels that do not change as well.
    pub show_all: bool,
    /// The stretch whose easing is being edited: its property and first key's frame.
    segment: Option<(String, TrackProperty, f32)>,
    drag: Option<KeyDrag>,
    /// A Bezier handle being dragged (0 or 1) and its merge key.
    handle: Option<(usize, String)>,
    gesture: u64,
}

pub fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::X => "Position X",
        Channel::Y => "Position Y",
        Channel::Z => "Position Z",
        Channel::RotX => "Tilt (rot X)",
        Channel::RotY => "Pan (rot Y)",
        Channel::RotZ => "Roll (rot Z)",
        Channel::ScaleX => "Scale X",
        Channel::ScaleY => "Scale Y",
        Channel::ScaleZ => "Scale Z",
        Channel::Focal => "Focal length",
    }
}

fn unit(c: Channel) -> &'static str {
    match c {
        Channel::X | Channel::Y | Channel::Z => "m",
        Channel::RotX | Channel::RotY | Channel::RotZ => "°",
        Channel::Focal => "mm",
        _ => "×",
    }
}

fn color(c: Channel) -> Color32 {
    match c {
        Channel::X => Color32::from_rgb(235, 90, 90),
        Channel::Y => Color32::from_rgb(110, 205, 95),
        Channel::Z => Color32::from_rgb(90, 150, 245),
        Channel::RotX => Color32::from_rgb(245, 160, 150),
        Channel::RotY => Color32::from_rgb(170, 230, 150),
        Channel::RotZ => Color32::from_rgb(150, 190, 255),
        Channel::ScaleX => Color32::from_rgb(170, 70, 70),
        Channel::ScaleY => Color32::from_rgb(80, 150, 70),
        Channel::ScaleZ => Color32::from_rgb(70, 100, 180),
        Channel::Focal => Color32::from_rgb(240, 200, 80),
    }
}

/// The easing presets the editor offers, with their API names.
pub const PRESETS: [(&str, Interp); 6] = [
    ("Smooth (ease)", Interp::Ease),
    ("Ease in", Interp::EaseIn),
    ("Ease out", Interp::EaseOut),
    ("Linear", Interp::Linear),
    ("Overshoot", Interp::Overshoot),
    ("Hold (step)", Interp::Step),
];

/// One plotted channel: its keys and a sampled line, values unwrapped for rotations.
struct Plot {
    property: TrackProperty,
    channel: Channel,
    keys: Vec<(f32, f32, Interp)>,
    line: Vec<(f32, f32)>,
    lo: f32,
    hi: f32,
}

/// The entity whose curves to show for a selection: the selection itself when it is animated,
/// else its group (`root`).
pub fn subject(scene: &Scene, selected: &str, root: &str) -> Option<String> {
    let animated = |id: &str| {
        scene
            .timeline
            .tracks
            .iter()
            .any(|t| t.entity == id && t.property != TrackProperty::Expression && !t.keys.is_empty())
    };
    [selected, root].into_iter().find(|id| animated(id)).map(str::to_string)
}

fn plots(scene: &Scene, id: &str, t0: f32, t1: f32, width: f32, all: bool) -> Vec<Plot> {
    let mut out = Vec::new();
    for property in [TrackProperty::Transform, TrackProperty::FocalLength] {
        let Some(track) = scene.timeline.track(id, property).filter(|t| !t.keys.is_empty()) else { continue };
        for &channel in Channel::of(property) {
            if let Some(p) = plot(track, property, channel, t0, t1, width)
                && (all || p.keys.iter().any(|k| (k.1 - p.keys[0].1).abs() > 1e-4))
            {
                out.push(p);
            }
        }
    }
    out
}

fn plot(track: &Track, property: TrackProperty, channel: Channel, t0: f32, t1: f32, width: f32) -> Option<Plot> {
    let n = (width / 3.0).clamp(16.0, 600.0) as usize;
    let rot = channel.is_rotation();
    let mut keys = Vec::new();
    let mut prev: Option<f32> = None;
    for k in &track.keys {
        let v = channel.get(&k.value)?;
        let v = if rot { prev.map_or(v, |p| unwrap_degrees(v, p)) } else { v };
        prev = Some(v);
        keys.push((k.frame, v, k.interp));
    }
    let mut line = Vec::with_capacity(n + 1);
    prev = keys.first().map(|k| k.1);
    for i in 0..=n {
        let f = t0 + (t1 - t0) * i as f32 / n as f32;
        let v = channel.get(&track.sample(f)?)?;
        let v = if rot { prev.map_or(v, |p| unwrap_degrees(v, p)) } else { v };
        prev = Some(v);
        line.push((f, v));
    }
    // line and keys share the unwrap: pull each key to the line's turn at its frame
    if rot {
        for k in &mut keys {
            if let Some(near) = line.iter().min_by(|a, b| (a.0 - k.0).abs().total_cmp(&(b.0 - k.0).abs())) {
                k.1 = unwrap_degrees(k.1, near.1);
            }
        }
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in line.iter().map(|p| p.1).chain(keys.iter().map(|k| k.1)) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    Some(Plot { property, channel, keys, line, lo, hi })
}

fn padded((lo, hi): (f32, f32)) -> (f32, f32) {
    let span = (hi - lo).max(1e-3);
    let pad = span * 0.12;
    if hi - lo < 1e-3 { (lo - 0.5, hi + 0.5) } else { (lo - pad, hi + pad) }
}

impl CurveEditor {
    /// Open the easing of the stretch that starts at `frame` on `id`'s `property` track.
    pub fn focus(&mut self, id: &str, property: TrackProperty, frame: f32) {
        self.segment = Some((id.to_string(), property, frame));
    }

    fn merge(&mut self, tag: &str) -> String {
        self.gesture += 1;
        format!("curve-{tag}:{}", self.gesture)
    }

    /// Draw the editor for `id` across the full width of `ui`: a channel list in the first
    /// `label_w` pixels, the curves over frames `t0..t1` beside it.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        scene: &Scene,
        id: &str,
        label_w: f32,
        (t0, t1): (f32, f32),
        now: f32,
        height: f32,
    ) -> Vec<CurveAction> {
        let mut out = Vec::new();
        let full = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(full, height), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, Color32::from_gray(20));
        let list = Rect::from_min_size(rect.min, Vec2::new(label_w, height));
        let graph = Rect::from_min_max(
            egui::pos2(rect.left() + label_w, rect.top() + 4.0),
            egui::pos2(rect.right(), rect.bottom() - 4.0),
        );
        let x_of = |f: f32| graph.left() + (f - t0) / (t1 - t0).max(1e-3) * graph.width();
        let f_of = |x: f32| t0 + (x - graph.left()) / graph.width().max(1.0) * (t1 - t0);

        let all = plots(scene, id, t0, t1, graph.width(), self.show_all);
        if all.is_empty() {
            p.text(
                graph.left_center() + Vec2::new(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                tr("Nothing changes yet: key this object at two frames with different poses."),
                egui::FontId::proportional(11.0),
                Color32::from_gray(120),
            );
        }
        // forget a segment that no longer exists
        if let Some((sid, prop, f)) = &self.segment
            && (sid != id
                || !scene.timeline.track(id, *prop).is_some_and(|t| {
                    t.keys.iter().position(|k| (k.frame - f).abs() < 1e-3).is_some_and(|i| i + 1 < t.keys.len())
                }))
        {
            self.segment = None;
        }

        // channel list: click a name to show or hide it
        let mut y = list.top() + 4.0;
        for pl in &all {
            let r = Rect::from_min_size(egui::pos2(list.left() + 4.0, y), Vec2::new(label_w - 8.0, 14.0));
            let on = !self.hidden.contains(&pl.channel);
            let c = if on { color(pl.channel) } else { Color32::from_gray(70) };
            p.rect_filled(Rect::from_center_size(egui::pos2(r.left() + 5.0, r.center().y), Vec2::splat(7.0)), 2.0, c);
            p.text(
                egui::pos2(r.left() + 13.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                tr(channel_name(pl.channel)),
                egui::FontId::proportional(10.5),
                if on { Color32::from_gray(200) } else { Color32::from_gray(100) },
            );
            if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| r.contains(q)) {
                if on {
                    self.hidden.insert(pl.channel);
                } else {
                    self.hidden.remove(&pl.channel);
                }
            }
            y += 14.0;
            if y > list.bottom() - 14.0 {
                break;
            }
        }
        let shown: Vec<&Plot> = all.iter().filter(|pl| !self.hidden.contains(&pl.channel)).collect();

        // each channel fills the height on its own scale (metres, degrees and millimetres do not
        // share one); a dragged channel keeps the scale it had when the drag began
        let dragged = self.drag.as_ref().map(|d| (d.channel, d.property, d.range));
        let range_of = |pl: &Plot| -> (f32, f32) {
            match dragged {
                Some((c, prop, r)) if c == pl.channel && prop == pl.property => r,
                _ => padded((pl.lo, pl.hi)),
            }
        };
        let y_of = |v: f32, (lo, hi): (f32, f32)| graph.bottom() - (v - lo) / (hi - lo).max(1e-6) * graph.height();
        let v_of = |y: f32, (lo, hi): (f32, f32)| lo + (graph.bottom() - y) / graph.height().max(1.0) * (hi - lo);

        // grid: frames (light) and the playhead
        let gp = ui.painter_at(graph.expand2(Vec2::new(0.0, 4.0)));
        for i in 1..4 {
            let yy = graph.top() + graph.height() * i as f32 / 4.0;
            gp.line_segment(
                [egui::pos2(graph.left(), yy), egui::pos2(graph.right(), yy)],
                (1.0, Color32::from_gray(30)),
            );
        }
        // the selected stretch, shaded
        if let Some((_, prop, f)) = &self.segment
            && let Some(t) = scene.timeline.track(id, *prop)
            && let Some(i) = t.keys.iter().position(|k| (k.frame - f).abs() < 1e-3)
            && let Some(next) = t.keys.get(i + 1)
        {
            let r = Rect::from_x_y_ranges(x_of(*f)..=x_of(next.frame), graph.y_range());
            gp.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(120, 160, 255, 22));
        }

        // pointer: the nearest key, else the nearest curve stretch
        let ease_rect = self.segment.as_ref().map(|(_, _, f)| {
            let size = Vec2::new(250.0f32.min(graph.width() * 0.5), (rect.height() - 24.0).max(60.0));
            let min = if x_of(*f) > graph.center().x {
                egui::pos2(graph.left() + 6.0, rect.top() + 22.0)
            } else {
                egui::pos2(graph.right() - size.x - 6.0, rect.top() + 22.0)
            };
            Rect::from_min_size(min, size)
        });
        let in_box = |q: Pos2| ease_rect.is_some_and(|r| r.contains(q));
        let hover = resp.hover_pos().filter(|h| graph.expand(6.0).contains(*h) && !in_box(*h));
        let mut near_key: Option<(usize, usize, f32)> = None; // plot, key, distance
        let mut near_line: Option<(usize, f32, f32)> = None; // plot, frame, distance
        if let Some(h) = hover {
            for (pi, pl) in shown.iter().enumerate() {
                let rg = range_of(pl);
                for (ki, k) in pl.keys.iter().enumerate() {
                    let d = (egui::pos2(x_of(k.0), y_of(k.1, rg)) - h).length();
                    if d < 7.0 && near_key.is_none_or(|n| d < n.2) {
                        near_key = Some((pi, ki, d));
                    }
                }
                for w in pl.line.windows(2) {
                    let (a, b) =
                        (egui::pos2(x_of(w[0].0), y_of(w[0].1, rg)), egui::pos2(x_of(w[1].0), y_of(w[1].1, rg)));
                    let d = dist_to_segment(h, a, b);
                    if d < 5.0 && near_line.is_none_or(|n| d < n.2) {
                        near_line = Some((pi, f_of(h.x), d));
                    }
                }
            }
        }

        // curves and keys
        for (pi, pl) in shown.iter().enumerate() {
            let rg = range_of(pl);
            let c = color(pl.channel);
            let hot = near_line.is_some_and(|n| n.0 == pi) || near_key.is_some_and(|n| n.0 == pi);
            let pts: Vec<Pos2> = pl.line.iter().map(|(f, v)| egui::pos2(x_of(*f), y_of(*v, rg))).collect();
            gp.add(egui::Shape::line(pts, (if hot { 2.2 } else { 1.4 }, c)));
            for (ki, k) in pl.keys.iter().enumerate() {
                if k.0 < t0 - 0.5 || k.0 > t1 + 0.5 {
                    continue;
                }
                let at = egui::pos2(x_of(k.0), y_of(k.1, rg));
                let active = near_key.is_some_and(|n| n.0 == pi && n.1 == ki)
                    || self.drag.as_ref().is_some_and(|d| d.channel == pl.channel && (d.frame - k.0).abs() < 0.5);
                gp.circle_filled(at, if active { 5.0 } else { 3.5 }, if active { Color32::WHITE } else { c });
                gp.circle_stroke(at, if active { 5.0 } else { 3.5 }, (1.0, Color32::from_gray(15)));
            }
        }
        let px = x_of(now);
        if (graph.left()..=graph.right()).contains(&px) {
            gp.line_segment(
                [egui::pos2(px, graph.top() - 4.0), egui::pos2(px, graph.bottom() + 4.0)],
                (1.5, Color32::from_rgb(255, 120, 90)),
            );
        }

        // hover readout
        if let Some((pi, ki, _)) = near_key {
            let (pl, k) = (shown[pi], shown[pi].keys[ki]);
            if self.drag.is_none() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            let txt = crate::lf!(
                "{} {:.2}{} · frame {:.0} · drag up/down to change, sideways to retime",
                "{} {:.2}{} · 第 {:.0} 帧 · 上下拖改数值，左右拖改时间",
                tr(channel_name(pl.channel)),
                k.1,
                unit(pl.channel),
                k.0
            );
            readout(&p, rect, &txt);
        } else if let Some((pi, f, _)) = near_line {
            let pl = shown[pi];
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            let v =
                pl.line.iter().min_by(|a, b| (a.0 - f).abs().total_cmp(&(b.0 - f).abs())).map(|p| p.1).unwrap_or(0.0);
            let txt = crate::lf!(
                "{} {:.2}{} · click to shape this stretch's easing, double-click to add a key",
                "{} {:.2}{} · 单击编辑这一段的缓动，双击添加关键帧",
                tr(channel_name(pl.channel)),
                v,
                unit(pl.channel)
            );
            readout(&p, rect, &txt);
        } else if let Some(d) = &self.drag {
            let txt = crate::lf!("{} · frame {:.0}", "{} · 第 {:.0} 帧", tr(channel_name(d.channel)), d.frame);
            readout(&p, rect, &txt);
        }

        // start a key drag
        if resp.drag_started()
            && let Some((pi, ki, _)) = near_key
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            let pl = shown[pi];
            let merge = self.merge("key");
            self.drag = Some(KeyDrag {
                property: pl.property,
                channel: pl.channel,
                frame: pl.keys[ki].0,
                from_frame: pl.keys[ki].0,
                from_value: pl.keys[ki].1,
                origin,
                sideways: None,
                range: range_of(pl),
                merge,
            });
        }
        if let (Some(mut d), Some(pos)) = (self.drag.clone(), ui.input(|i| i.pointer.latest_pos()))
            && (resp.dragged() || resp.drag_stopped())
        {
            let delta = pos - d.origin;
            if d.sideways.is_none() && delta.length() > 4.0 {
                d.sideways = Some(delta.x.abs() > delta.y.abs());
            }
            match d.sideways {
                Some(true) => {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    let to = f_of(pos.x).round().max(0.0);
                    let taken = scene
                        .timeline
                        .track(id, d.property)
                        .is_some_and(|t| t.keys.iter().any(|k| (k.frame - to).abs() < 0.5));
                    if (to - d.frame).abs() >= 1.0 && !taken {
                        out.push(CurveAction::Run {
                            merge: Some(d.merge.clone()),
                            command: "keyframe.move",
                            params: json!({"id": id, "property": d.property, "from": d.frame, "to": to}),
                        });
                        if let Some((_, _, sf)) = &mut self.segment
                            && (*sf - d.frame).abs() < 1e-3
                        {
                            *sf = to;
                        }
                        d.frame = to;
                        out.push(CurveAction::SetFrame(to));
                    }
                }
                Some(false) => {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
                    let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
                    let dv = (v_of(pos.y, d.range) - v_of(d.origin.y, d.range)) * fine;
                    let mut value = d.from_value + dv;
                    if d.channel == Channel::Focal {
                        value = value.clamp(1.0, 2000.0);
                    }
                    out.push(CurveAction::Run {
                        merge: Some(d.merge.clone()),
                        command: "keyframe.setChannel",
                        params: json!({"id": id, "property": d.property, "frame": d.frame, "channel": d.channel, "value": value}),
                    });
                }
                None => {}
            }
            self.drag = if resp.drag_stopped() {
                if d.sideways.is_none() {
                    // a press without a move: go to the key
                    out.push(CurveAction::SetFrame(d.from_frame));
                }
                None
            } else {
                Some(d)
            };
        }

        // click: a stretch selects its easing; elsewhere moves the playhead
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && graph.contains(pos)
            && !in_box(pos)
        {
            if let Some((pi, ki, _)) = near_key {
                out.push(CurveAction::SetFrame(shown[pi].keys[ki].0));
            } else if let Some((pi, f, _)) = near_line {
                let pl = shown[pi];
                let start = pl.keys.iter().rev().find(|k| k.0 <= f).map(|k| k.0);
                let has_next = start.is_some_and(|s| pl.keys.iter().any(|k| k.0 > s));
                self.segment = start.filter(|_| has_next).map(|s| (id.to_string(), pl.property, s));
            } else {
                self.segment = None;
                out.push(CurveAction::SetFrame(f_of(pos.x).round().clamp(t0, t1)));
            }
        }
        // double-click a curve: a new key there, on the curve (nothing jumps)
        if resp.double_clicked()
            && let Some((pi, f, _)) = near_line
            && near_key.is_none()
        {
            let pl = shown[pi];
            let f = f.round().max(0.0);
            if let Some(track) = scene.timeline.track(id, pl.property)
                && let Some(value) = track.sample(f)
                && !track.keys.iter().any(|k| (k.frame - f).abs() < 0.5)
            {
                let interp = track.keys.iter().rev().find(|k| k.frame <= f).map(|k| k.interp).unwrap_or_default();
                out.push(CurveAction::Run {
                    merge: None,
                    command: "keyframe.set",
                    params: json!({"id": id, "property": pl.property, "frame": f, "value": value, "interp": interp}),
                });
                out.push(CurveAction::SetFrame(f));
            }
        }

        // the easing of the selected stretch, in a box on the side away from it
        if let Some((_, prop, f)) = self.segment.clone()
            && let Some(interp) = scene
                .timeline
                .track(id, prop)
                .and_then(|t| t.keys.iter().find(|k| (k.frame - f).abs() < 1e-3))
                .map(|k| k.interp)
            && let Some(er) = ease_rect
        {
            self.ease_box(ui, er, id, prop, f, interp, &mut out);
        }

        // "show all" toggle, bottom left
        let tip =
            Rect::from_min_size(egui::pos2(list.left() + 4.0, list.bottom() - 16.0), Vec2::new(label_w - 8.0, 14.0));
        let label = if self.show_all { tr("Changing only") } else { tr("Show all") };
        p.text(
            tip.left_center(),
            egui::Align2::LEFT_CENTER,
            &label,
            egui::FontId::proportional(10.0),
            Color32::from_rgb(130, 170, 230),
        );
        if resp.clicked() && resp.interact_pointer_pos().is_some_and(|q| tip.contains(q)) {
            self.show_all = !self.show_all;
        }
        out
    }

    /// The Bezier handles of one stretch, with the presets under them.
    #[allow(clippy::too_many_arguments)]
    fn ease_box(
        &mut self,
        ui: &mut egui::Ui,
        er: Rect,
        id: &str,
        prop: TrackProperty,
        frame: f32,
        interp: Interp,
        out: &mut Vec<CurveAction>,
    ) {
        let p = ui.painter_at(er.expand(1.0));
        p.rect_filled(er, 4.0, Color32::from_gray(30));
        p.rect_stroke(er, 4.0, (1.0, Color32::from_gray(60)), egui::StrokeKind::Inside);
        p.text(
            er.left_top() + Vec2::new(8.0, 4.0),
            egui::Align2::LEFT_TOP,
            crate::lf!("Easing from frame {frame:.0}", "第 {frame:.0} 帧起的缓动"),
            egui::FontId::proportional(10.5),
            Color32::from_gray(190),
        );
        // the curve square on the left, the presets in a column on the right
        let side = (er.height() - 30.0).max(30.0);
        let sq = Rect::from_min_size(egui::pos2(er.left() + 14.0, er.top() + 22.0), Vec2::splat(side));
        // the box spans progress -0.5..1.5 vertically so overshoot fits
        let to_px =
            |x: f32, y: f32| egui::pos2(sq.left() + x * sq.width(), sq.bottom() - (y + 0.25) / 1.5 * sq.height());
        let from_px = |q: Pos2| ((q.x - sq.left()) / sq.width(), (sq.bottom() - q.y) / sq.height() * 1.5 - 0.25);
        p.rect_filled(sq, 2.0, Color32::from_gray(18));
        p.line_segment([to_px(0.0, 0.0), to_px(1.0, 0.0)], (1.0, Color32::from_gray(45)));
        p.line_segment([to_px(0.0, 1.0), to_px(1.0, 1.0)], (1.0, Color32::from_gray(45)));
        let pts: Vec<Pos2> = (0..=48).map(|i| i as f32 / 48.0).map(|t| to_px(t, interp.apply(t))).collect();
        p.add(egui::Shape::line(pts, (2.0, crate::ui::ACCENT)));
        let editable = interp.as_bezier();
        if let Some(h) = editable {
            let handles = [to_px(h[0], h[1]), to_px(h[2], h[3])];
            p.line_segment([to_px(0.0, 0.0), handles[0]], (1.0, Color32::from_gray(150)));
            p.line_segment([to_px(1.0, 1.0), handles[1]], (1.0, Color32::from_gray(150)));
            let resp =
                ui.interact(sq.expand(8.0), ui.scope_id().with(("ease_handles", id, frame as i64)), Sense::drag());
            let pointer = resp.hover_pos();
            let near = |q: Pos2| handles.iter().position(|hh| (*hh - q).length() < 9.0);
            for (i, hh) in handles.iter().enumerate() {
                let hot = pointer.and_then(near) == Some(i) || self.handle.as_ref().is_some_and(|(j, _)| *j == i);
                p.circle_filled(
                    *hh,
                    if hot { 5.5 } else { 4.5 },
                    if hot { Color32::WHITE } else { Color32::from_gray(200) },
                );
            }
            if pointer.and_then(near).is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            if resp.drag_started()
                && let Some(i) = ui.input(|i| i.pointer.press_origin()).and_then(near)
            {
                let m = self.merge("ease");
                self.handle = Some((i, m));
            }
            if let (Some((i, m)), Some(q)) = (self.handle.clone(), resp.interact_pointer_pos())
                && resp.dragged()
            {
                let (x, y) = from_px(q);
                let mut nh = h;
                nh[i * 2] = (x * 100.0).round() / 100.0;
                nh[i * 2 + 1] = (y * 100.0).round() / 100.0;
                let nh = Interp::clamp_handles(nh);
                out.push(CurveAction::Run {
                    merge: Some(m),
                    command: "keyframe.setInterp",
                    params: json!({"id": id, "property": prop, "frame": frame, "interp": Interp::Bezier(nh)}),
                });
            }
            if resp.drag_stopped() {
                self.handle = None;
            }
            p.text(
                egui::pos2(er.right() - 8.0, er.top() + 5.0),
                egui::Align2::RIGHT_TOP,
                format!("({:.2}, {:.2}, {:.2}, {:.2})", h[0], h[1], h[2], h[3]),
                egui::FontId::monospace(9.5),
                Color32::from_gray(140),
            );
        }
        // presets: a column of small buttons
        let x = sq.right() + 14.0;
        let mut y = er.top() + 22.0;
        let row = ((er.bottom() - 4.0 - y) / PRESETS.len() as f32).clamp(12.0, 18.0);
        for (label, preset) in PRESETS {
            let text = tr(label);
            let g = p.layout_no_wrap(text, egui::FontId::proportional(10.0), Color32::from_gray(210));
            let r = Rect::from_min_size(egui::pos2(x, y), Vec2::new(er.right() - 8.0 - x, row - 2.0));
            y += row;
            let resp = ui.interact(r, ui.scope_id().with(("ease_preset", label)), Sense::click());
            let on = interp == preset;
            p.rect_filled(
                r,
                3.0,
                if on {
                    crate::ui::ACCENT.gamma_multiply(0.6)
                } else if resp.hovered() {
                    Color32::from_gray(60)
                } else {
                    Color32::from_gray(40)
                },
            );
            p.galley(egui::pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(220));
            if resp.clicked() {
                out.push(CurveAction::Run {
                    merge: None,
                    command: "keyframe.setInterp",
                    params: json!({"id": id, "property": prop, "frame": frame, "interp": preset}),
                });
            }
        }
        if editable.is_none() {
            p.text(
                sq.center(),
                egui::Align2::CENTER_CENTER,
                tr("Holds, then jumps"),
                egui::FontId::proportional(10.5),
                Color32::from_gray(160),
            );
        }
    }
}

fn readout(p: &egui::Painter, rect: Rect, text: &str) {
    let g = p.layout_no_wrap(text.to_string(), egui::FontId::proportional(10.5), Color32::from_gray(220));
    let r = Rect::from_min_size(egui::pos2(rect.left() + 136.0, rect.top() + 2.0), g.size() + Vec2::new(8.0, 4.0));
    p.rect_filled(r, 3.0, Color32::from_black_alpha(170));
    p.galley(r.min + Vec2::new(4.0, 2.0), g, Color32::WHITE);
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotstage_core::{Key, KeyValue, Transform};

    #[test]
    fn rotation_curves_stay_continuous() {
        // a pan from 170° to 190° crosses ±180: the plot must not jump a whole turn
        let mut t = Track { entity: "cam".into(), property: TrackProperty::Transform, keys: vec![] };
        for (f, deg) in [(0.0, 170.0f32), (10.0, 190.0)] {
            let mut tr = Transform::default();
            tr.set_euler_degrees([0.0, deg, 0.0]);
            t.set_key(Key { frame: f, value: KeyValue::Transform(tr), interp: Interp::Linear });
        }
        let pl = plot(&t, TrackProperty::Transform, Channel::RotY, 0.0, 10.0, 300.0).unwrap();
        for w in pl.line.windows(2) {
            assert!((w[1].1 - w[0].1).abs() < 10.0, "jump {:?}", w);
        }
        assert!((pl.keys[1].1 - pl.keys[0].1 - 20.0).abs() < 0.5, "{:?}", pl.keys);
    }
}
