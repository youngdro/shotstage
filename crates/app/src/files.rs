//! Opening and saving files: native dialogs on the desktop, the browser's file picker,
//! drag-and-drop and downloads on the web. Picked and dropped files land in an [`Inbox`] that
//! the editor imports from on its next frame.

use std::cell::RefCell;
use std::rc::Rc;

use egui::Pos2;

/// Files waiting to be imported: (file name, bytes, where they were dropped).
pub type Inbox = Rc<RefCell<Vec<(String, Vec<u8>, Option<Pos2>)>>>;

/// File extensions the editor can import.
pub const IMPORTABLE: &[&str] =
    &["glb", "gltf", "obj", "stl", "fbx", "blend", "png", "jpg", "jpeg", "webp", "hdr", "json"];

/// Ask for files to import.
pub fn pick(inbox: &Inbox, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ctx;
        if let Some(paths) = rfd::FileDialog::new().add_filter("Models, images and HDRIs", IMPORTABLE).pick_files() {
            for p in paths {
                if let Ok(bytes) = std::fs::read(&p) {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("model").to_string();
                    inbox.borrow_mut().push((name, bytes, None));
                }
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let (inbox, ctx) = (inbox.clone(), ctx.clone());
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(files) =
                rfd::AsyncFileDialog::new().add_filter("Models, images and HDRIs", IMPORTABLE).pick_files().await
            {
                for f in files {
                    let bytes = f.read().await;
                    inbox.borrow_mut().push((f.file_name(), bytes, None));
                }
                ctx.request_repaint();
            }
        });
    }
}

/// Queue files dropped onto the window (at the pointer position).
pub fn take_dropped(ctx: &egui::Context, inbox: &Inbox) {
    let (dropped, at) = ctx.input(|i| (i.raw.dropped_files.clone(), i.pointer.hover_pos()));
    for f in dropped {
        let name = f.path().file_name().and_then(|n| n.to_str()).unwrap_or("model").to_string();
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(bytes) = f.bytes() {
            inbox.borrow_mut().push((name, bytes, at));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (inbox, ctx) = (inbox.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(bytes) = f.bytes_async().await {
                    inbox.borrow_mut().push((name, bytes, at));
                    ctx.request_repaint();
                }
            });
        }
    }
}

/// Save bytes as a file: a save dialog on the desktop, a download in the browser. Returns
/// where it went, or `None` when cancelled.
pub fn save(name: &str, bytes: &[u8], mime: &str) -> Result<Option<String>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = mime;
        let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() else { return Ok(None) };
        std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(Some(path.display().to_string()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type(mime);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(err)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let a: web_sys::HtmlAnchorElement =
            doc.create_element("a").map_err(err)?.dyn_into().map_err(|_| "no anchor")?;
        a.set_href(&url);
        a.set_download(name);
        a.click();
        let _ = web_sys::Url::revoke_object_url(&url);
        Ok(Some(format!("downloaded {name}")))
    }
}

/// Where the result of a background save lands for the editor to report: Ok(where) / Err(why).
pub type Outbox = Rc<RefCell<Option<Result<String, String>>>>;

/// Render the shot camera's view at the current frame and save it as a PNG.
pub fn save_frame(hub: &crate::hub::SharedHub, width: u32, done: &Outbox) {
    let frame = hub.borrow().frame;
    let name = format!("shotstage_frame_{:04}.png", frame.round() as i64);
    let params = serde_json::json!({ "width": width });
    #[cfg(not(target_arch = "wasm32"))]
    {
        use base64::Engine as _;
        let r = crate::hub::dispatch(hub, serde_json::json!({ "command": "render.snapshot", "params": params }))
            .map_err(|e| e.message)
            .and_then(|o| {
                let b64 = o.result["data"].as_str().unwrap_or_default().to_string();
                base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())
            })
            .and_then(|png| save(&name, &png, "image/png"));
        *done.borrow_mut() = match r {
            Ok(Some(w)) => Some(Ok(w)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        };
    }
    #[cfg(target_arch = "wasm32")]
    {
        struct SingleThread<T>(T);
        // SAFETY: wasm32 without atomics has a single thread; the value is only used on it.
        unsafe impl<T> Send for SingleThread<T> {}
        impl<T> SingleThread<T> {
            // a method call moves the whole wrapper into the closure (not just its fields)
            fn into_inner(self) -> T {
                self.0
            }
        }
        let drawn = hub.borrow_mut().draw_snapshot(params);
        let (_, rs, rb) = match drawn {
            Ok(x) => x,
            Err(e) => {
                *done.borrow_mut() = Some(Err(e.message));
                return;
            }
        };
        hub.borrow_mut().pending_snapshots += 1;
        let buffer = rb.buffer.clone();
        let cell = SingleThread((hub.clone(), rb, done.clone(), name));
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let (hub, rb, done, name) = cell.into_inner();
            hub.borrow_mut().pending_snapshots -= 1;
            let out = r
                .map_err(|e| e.to_string())
                .and_then(|_| rb.rgba())
                .and_then(|rgba| shotstage_render::encode_png(&rgba, rb.width, rb.height))
                .and_then(|png| save(&name, &png, "image/png"));
            *done.borrow_mut() = match out {
                Ok(Some(w)) => Some(Ok(w)),
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            };
            if let Some(ctx) = &hub.borrow().repaint {
                ctx.request_repaint();
            }
        });
        let _ = rs.device.poll(wgpu::PollType::Poll);
    }
}

/// One storyboard panel: heading, a line under it, and where to look (camera, frame).
pub struct Panel {
    pub heading: String,
    pub note: String,
    pub camera: String,
    pub frame: f32,
}

/// A single HTML page with every panel's picture embedded, to open, print or send.
fn board_html(title: &str, panels: &[Panel], pngs: &[Vec<u8>]) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut cards = String::new();
    for (p, png) in panels.iter().zip(pngs) {
        cards.push_str(&format!(
            "<figure><img src=\"data:image/png;base64,{}\" alt=\"{}\"><figcaption><b>{}</b><span>{}</span></figcaption></figure>\n",
            shotstage_commands::model::encode(png),
            esc(&p.heading),
            esc(&p.heading),
            esc(&p.note)
        ));
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<title>{t}</title><style>body{{font:14px system-ui,sans-serif;margin:24px;background:#fff;color:#111}}\
h1{{font-size:20px;margin:0 0 16px}}main{{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:20px}}\
figure{{margin:0}}img{{width:100%;display:block;border:1px solid #ccc;border-radius:4px}}\
figcaption{{margin-top:6px;display:flex;flex-direction:column;gap:2px}}figcaption span{{color:#666}}\
@media print{{body{{margin:0}}main{{gap:12px}}}}</style></head><body><h1>{t}</h1><main>\n{cards}</main></body></html>",
        t = esc(title)
    )
}

/// Render every panel through its camera and save one storyboard HTML page.
pub fn save_storyboard(hub: &crate::hub::SharedHub, panels: Vec<Panel>, width: u32, done: &Outbox) {
    let shots: Vec<serde_json::Value> =
        panels.iter().map(|p| serde_json::json!({ "width": width, "camera": p.camera, "frame": p.frame })).collect();
    save_rendered(hub, shots, done, move |pngs| {
        Ok(("storyboard.html".into(), board_html("Storyboard", &panels, &pngs).into_bytes(), "text/html"))
    });
}

/// Render a shot sheet (`render.shotSheet` params) and save the PDF / PNG.
pub fn save_shot_sheet(hub: &crate::hub::SharedHub, params: serde_json::Value, done: &Outbox) {
    use shotstage_commands::sheet;
    let plan = match sheet::sheet_plan(hub.borrow().session.scene(), params) {
        Ok(p) => p,
        Err(e) => {
            *done.borrow_mut() = Some(Err(e.message));
            return;
        }
    };
    let shots = plan.pictures.clone();
    save_rendered(hub, shots, done, move |pngs| {
        let bytes = plan.encode(&sheet::compose(&plan, &pngs)).map_err(|e| e.message)?;
        Ok((plan.file_name().to_string(), bytes, plan.mime()))
    });
}

type Finish = (String, Vec<u8>, &'static str);

/// Render `shots` (`render.snapshot` params), make a file of the pictures with `finish` and
/// save it; the outcome lands in `done`. Synchronous on the desktop, picture by picture as the
/// GPU finishes in the browser.
fn save_rendered(
    hub: &crate::hub::SharedHub,
    shots: Vec<serde_json::Value>,
    done: &Outbox,
    finish: impl FnOnce(Vec<Vec<u8>>) -> Result<Finish, String> + 'static,
) {
    let deliver = |r: Result<Finish, String>| match r.and_then(|(name, bytes, mime)| save(&name, &bytes, mime)) {
        Ok(Some(w)) => Some(Ok(w)),
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use base64::Engine as _;
        let mut pngs = Vec::new();
        for p in shots {
            let r = crate::hub::dispatch(hub, serde_json::json!({ "command": "render.snapshot", "params": p }))
                .map_err(|e| e.message)
                .and_then(|o| {
                    let b64 = o.result["data"].as_str().unwrap_or_default().to_string();
                    base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())
                });
            match r {
                Ok(png) => pngs.push(png),
                Err(e) => {
                    *done.borrow_mut() = Some(Err(e));
                    return;
                }
            }
        }
        *done.borrow_mut() = deliver(finish(pngs));
    }
    #[cfg(target_arch = "wasm32")]
    {
        use std::cell::RefCell;
        use std::rc::Rc;
        struct SingleThread<T>(T);
        // SAFETY: wasm32 without atomics has a single thread; the value is only used on it.
        unsafe impl<T> Send for SingleThread<T> {}
        impl<T> SingleThread<T> {
            fn into_inner(self) -> T {
                self.0
            }
        }
        type Pending = (
            Vec<Option<Vec<u8>>>,
            usize,
            Option<String>,
            Option<Box<dyn FnOnce(Vec<Vec<u8>>) -> Result<Finish, String>>>,
        );
        // the pictures arrive one by one; the file is made when the last one is in
        let n = shots.len();
        let state: Rc<RefCell<Pending>> = Rc::new(RefCell::new((vec![None; n], n, None, Some(Box::new(finish)))));
        if n == 0 {
            let f = state.borrow_mut().3.take().expect("set above");
            *done.borrow_mut() = deliver(f(vec![]));
            return;
        }
        for (i, p) in shots.into_iter().enumerate() {
            let drawn = hub.borrow_mut().draw_snapshot(p);
            let (_, rs, rb) = match drawn {
                Ok(x) => x,
                Err(e) => {
                    *done.borrow_mut() = Some(Err(e.message));
                    return;
                }
            };
            hub.borrow_mut().pending_snapshots += 1;
            let buffer = rb.buffer.clone();
            let cell = SingleThread((hub.clone(), rb, done.clone(), state.clone()));
            buffer.map_async(wgpu::MapMode::Read, .., move |r| {
                let (hub, rb, done, state) = cell.into_inner();
                hub.borrow_mut().pending_snapshots -= 1;
                let png = r
                    .map_err(|e| e.to_string())
                    .and_then(|_| rb.rgba())
                    .and_then(|rgba| shotstage_render::encode_png(&rgba, rb.width, rb.height));
                let mut st = state.borrow_mut();
                match png {
                    Ok(p) => st.0[i] = Some(p),
                    Err(e) => st.2 = Some(e),
                }
                st.1 -= 1;
                if st.1 == 0 {
                    let out = match st.2.take() {
                        Some(e) => Err(e),
                        None => {
                            let pngs: Vec<Vec<u8>> = st.0.iter_mut().map(|p| p.take().unwrap_or_default()).collect();
                            match st.3.take() {
                                Some(f) => f(pngs),
                                None => Err("already saved".into()),
                            }
                        }
                    };
                    *done.borrow_mut() = match out.and_then(|(name, bytes, mime)| save(&name, &bytes, mime)) {
                        Ok(Some(w)) => Some(Ok(w)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    };
                }
                if let Some(ctx) = &hub.borrow().repaint {
                    ctx.request_repaint();
                }
            });
            let _ = rs.device.poll(wgpu::PollType::Poll);
        }
    }
}

/// A video being exported: frames rendered of all of them; set `cancel` to stop it.
#[derive(Clone, Debug, Default)]
pub struct VideoJob {
    pub done: usize,
    pub total: usize,
    pub cancel: bool,
}

pub type VideoProgress = Rc<RefCell<Option<VideoJob>>>;

/// Render `render.sequence` params (one pass) as an MP4 and save it as `name`: encoded in the
/// browser (WebCodecs), with ffmpeg on the desktop. `progress` follows it; the outcome lands in
/// `done`.
pub fn save_video(
    hub: &crate::hub::SharedHub,
    params: serde_json::Value,
    name: String,
    progress: &VideoProgress,
    done: &Outbox,
) {
    let mut params = params;
    params["video"] = serde_json::json!(true);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = rfd::FileDialog::new().set_file_name(&name).add_filter("MP4", &["mp4"]).save_file() else {
            return;
        };
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!("shotstage-video-{}-{stamp}", std::process::id()));
        params["out"] = serde_json::json!(tmp.display().to_string());
        *progress.borrow_mut() = None;
        let r = crate::hub::dispatch(hub, serde_json::json!({ "command": "render.sequence", "params": params }))
            .map_err(|e| e.message)
            .and_then(|o| {
                let made = o.result["videos"].get(0).and_then(|v| v.as_str()).map(String::from);
                let why =
                    o.result["warnings"].get(0).and_then(|v| v.as_str()).unwrap_or("no video was made").to_string();
                let made = made.ok_or(why)?;
                std::fs::copy(&made, &path).map_err(|e| format!("writing {}: {e}", path.display()))?;
                Ok(path.display().to_string())
            });
        let _ = std::fs::remove_dir_all(&tmp);
        *done.borrow_mut() = Some(r);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let (hub, progress, done) = (hub.clone(), progress.clone(), done.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let scene = hub.borrow().session.scene().clone();
            let r = match shotstage_commands::host::sequence_plan(&scene, params, 1280) {
                Err(e) => Err(e.message),
                Ok(plan) => {
                    *progress.borrow_mut() = Some(VideoJob { total: plan.frames.len(), ..Default::default() });
                    encode_video(&hub, &plan, 0, Some(&progress)).await
                }
            };
            let cancelled = progress.borrow().as_ref().is_some_and(|j| j.cancel);
            *progress.borrow_mut() = None;
            *done.borrow_mut() = match r {
                _ if cancelled => None,
                Ok(mp4) => match save(&name, &mp4, "video/mp4") {
                    Ok(Some(w)) => Some(Ok(w)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                },
                Err(e) => Some(Err(e)),
            };
            if let Some(ctx) = &hub.borrow().repaint {
                ctx.request_repaint();
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
mod video_js {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(module = "/js/video.js")]
    extern "C" {
        #[wasm_bindgen(catch, js_name = openVideo)]
        pub async fn open_video(width: u32, height: u32, fps: f64, bitrate: f64) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(catch, js_name = videoAdd)]
        pub async fn video_add(v: &JsValue, rgba: &js_sys::Uint8Array, index: u32) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(catch, js_name = videoFinish)]
        pub async fn video_finish(v: &JsValue) -> Result<JsValue, JsValue>;
        #[wasm_bindgen(js_name = videoCancel)]
        pub fn video_cancel(v: &JsValue);
    }
}

#[cfg(target_arch = "wasm32")]
fn js_message(e: wasm_bindgen::JsValue) -> String {
    js_sys::Reflect::get(&e, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

/// Render one picture (`render.snapshot` params) and read its RGBA pixels back.
#[cfg(target_arch = "wasm32")]
async fn render_rgba(hub: &crate::hub::SharedHub, params: serde_json::Value) -> Result<js_sys::Uint8Array, String> {
    use wasm_bindgen::{JsCast, JsValue};
    struct SingleThread<T>(T);
    // SAFETY: wasm32 without atomics has a single thread; the value is only used on it.
    unsafe impl<T> Send for SingleThread<T> {}
    impl<T> SingleThread<T> {
        fn into_inner(self) -> T {
            self.0
        }
    }
    let drawn = hub.borrow_mut().draw_snapshot(params);
    let (_, rs, rb) = drawn.map_err(|e| e.message)?;
    hub.borrow_mut().pending_snapshots += 1;
    let mut rb = Some(rb);
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let rb = rb.take().expect("the executor runs once");
        let buffer = rb.buffer.clone();
        let cell = SingleThread((hub.clone(), rb, resolve, reject));
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let (hub, rb, resolve, reject) = cell.into_inner();
            hub.borrow_mut().pending_snapshots -= 1;
            match r.map_err(|e| e.to_string()).and_then(|_| rb.rgba()) {
                Ok(px) => resolve.call1(&JsValue::NULL, &js_sys::Uint8Array::from(&px[..])),
                Err(e) => reject.call1(&JsValue::NULL, &JsValue::from_str(&e)),
            }
            .ok();
        });
        let _ = rs.device.poll(wgpu::PollType::Poll);
    });
    if let Some(ctx) = &hub.borrow().repaint {
        ctx.request_repaint();
    }
    let px = wasm_bindgen_futures::JsFuture::from(promise).await.map_err(js_message)?;
    Ok(px.unchecked_into())
}

/// Encode pass `pass` of a planned sequence (planned with `video`) as an MP4 in the browser.
#[cfg(target_arch = "wasm32")]
pub async fn encode_video(
    hub: &crate::hub::SharedHub,
    plan: &shotstage_commands::host::SequencePlan,
    pass: usize,
    progress: Option<&VideoProgress>,
) -> Result<Vec<u8>, String> {
    use wasm_bindgen::JsCast;
    let first = plan.frames.first().ok_or("no frames to render")?;
    let snap = &first.snapshots.get(pass).ok_or("no such pass")?.1;
    let (w, h) = (snap["width"].as_u64().unwrap_or(1280) as u32, snap["height"].as_u64().unwrap_or(720) as u32);
    let step = match plan.frames.as_slice() {
        [a, b, ..] => (b.frame - a.frame).max(1),
        _ => 1,
    };
    let fps = (plan.fps / step as f32).max(1.0) as f64;
    let bitrate = (w as f64 * h as f64 * fps * 0.15).clamp(1.0e6, 40.0e6);
    let enc = video_js::open_video(w, h, fps, bitrate).await.map_err(js_message)?;
    for (i, f) in plan.frames.iter().enumerate() {
        if progress.is_some_and(|p| p.borrow().as_ref().is_some_and(|j| j.cancel)) {
            video_js::video_cancel(&enc);
            return Err("cancelled".into());
        }
        let px = match render_rgba(hub, f.snapshots[pass].1.clone()).await {
            Ok(px) => px,
            Err(e) => {
                video_js::video_cancel(&enc);
                return Err(e);
            }
        };
        video_js::video_add(&enc, &px, i as u32).await.map_err(js_message)?;
        if let Some(p) = progress
            && let Some(j) = p.borrow_mut().as_mut()
        {
            j.done = i + 1;
        }
    }
    let out = video_js::video_finish(&enc).await.map_err(js_message)?;
    let get = |k: &str| js_sys::Reflect::get(&out, &k.into()).unwrap_or_default();
    let codec = match get("kind").as_string().as_deref() {
        Some("avc") => {
            let d = get("description");
            if d.is_undefined() || d.is_null() {
                return Err("the H.264 encoder gave no decoder configuration".into());
            }
            crate::mp4::Codec::Avc(d.unchecked_into::<js_sys::Uint8Array>().to_vec())
        }
        _ => crate::mp4::Codec::Vp9,
    };
    let chunks: js_sys::Array = get("chunks").unchecked_into();
    let samples: Vec<crate::mp4::Sample> = chunks
        .iter()
        .map(|c| crate::mp4::Sample {
            data: js_sys::Reflect::get(&c, &"data".into())
                .map(|d| d.unchecked_into::<js_sys::Uint8Array>().to_vec())
                .unwrap_or_default(),
            key: js_sys::Reflect::get(&c, &"key".into()).ok().and_then(|k| k.as_bool()).unwrap_or(false),
        })
        .collect();
    Ok(crate::mp4::write(&codec, w, h, fps as f32, &samples))
}
