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

/// Where the autosave lives: browser storage on the web, a file in the home folder on desktop.
pub fn autosave_write(json: &str) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("no home folder")?.join(".shotstage");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join("autosave.json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("autosave.json")).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let store = web_sys::window().and_then(|w| w.local_storage().ok().flatten()).ok_or("no local storage")?;
        store.set_item("shotstage.autosave", json).map_err(|_| "the scene is too big for browser storage".to_string())
    }
}

/// The last autosaved scene, if any.
pub fn autosave_read() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let p = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".shotstage").join("autosave.json");
        std::fs::read_to_string(p).ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()?.local_storage().ok()??.get_item("shotstage.autosave").ok()?
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
    let name = "storyboard.html";
    let title = "Storyboard";
    let params = |p: &Panel| serde_json::json!({ "width": width, "camera": p.camera, "frame": p.frame });
    #[cfg(not(target_arch = "wasm32"))]
    {
        use base64::Engine as _;
        let mut pngs = Vec::new();
        for p in &panels {
            let r = crate::hub::dispatch(hub, serde_json::json!({ "command": "render.snapshot", "params": params(p) }))
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
        let html = board_html(title, &panels, &pngs);
        *done.borrow_mut() = match save(name, html.as_bytes(), "text/html") {
            Ok(Some(w)) => Some(Ok(w)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        };
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
        // the pictures arrive one by one; the page is written when the last one is in
        let n = panels.len();
        let state = Rc::new(RefCell::new((vec![None::<Vec<u8>>; n], n, None::<String>)));
        let panels = Rc::new(panels);
        for i in 0..n {
            let drawn = hub.borrow_mut().draw_snapshot(params(&panels[i]));
            let (_, rs, rb) = match drawn {
                Ok(x) => x,
                Err(e) => {
                    *done.borrow_mut() = Some(Err(e.message));
                    return;
                }
            };
            hub.borrow_mut().pending_snapshots += 1;
            let buffer = rb.buffer.clone();
            let cell = SingleThread((hub.clone(), rb, done.clone(), state.clone(), panels.clone()));
            buffer.map_async(wgpu::MapMode::Read, .., move |r| {
                let (hub, rb, done, state, panels) = cell.into_inner();
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
                    *done.borrow_mut() = match st.2.take() {
                        Some(e) => Some(Err(e)),
                        None => {
                            let pngs: Vec<Vec<u8>> = st.0.iter_mut().map(|p| p.take().unwrap_or_default()).collect();
                            match save(name, board_html(title, &panels, &pngs).as_bytes(), "text/html") {
                                Ok(Some(w)) => Some(Ok(w)),
                                Ok(None) => None,
                                Err(e) => Some(Err(e)),
                            }
                        }
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

/// A file fetched over HTTP: still on its way, its contents (base64), or why it failed.
#[derive(Clone)]
pub enum Fetched {
    Loading,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Ready(Rc<String>),
    Failed(String),
}

/// Person files fetched so far, by file name.
pub type PeopleCache = Rc<RefCell<std::collections::HashMap<&'static str, Fetched>>>;

/// The GLB of a detailed person, base64: fetched from `people/<file>` next to the page the
/// first time it is asked for (the desktop carries them, see `people::bundled`).
pub fn person_file(cache: &PeopleCache, file: &'static str, hub: &crate::hub::SharedHub) -> Fetched {
    if let Some(f) = cache.borrow().get(file) {
        return f.clone();
    }
    cache.borrow_mut().insert(file, Fetched::Loading);
    fetch_person(cache.clone(), file, hub.clone());
    Fetched::Loading
}

/// GET `url` (relative to the page) and return the body.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let window = web_sys::window().ok_or("no window")?;
    let resp = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format!("{e:?}"))?
        .dyn_into::<web_sys::Response>()
        .map_err(|_| "not a response".to_string())?;
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

#[cfg(target_arch = "wasm32")]
fn fetch_person(cache: PeopleCache, file: &'static str, hub: crate::hub::SharedHub) {
    wasm_bindgen_futures::spawn_local(async move {
        let entry = match fetch_bytes(&format!("people/{file}")).await {
            Ok(bytes) => Fetched::Ready(Rc::new(shotstage_commands::model::encode(&bytes))),
            Err(e) => Fetched::Failed(e),
        };
        cache.borrow_mut().insert(file, entry);
        if let Some(ctx) = &hub.borrow().repaint {
            ctx.request_repaint();
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_person(cache: PeopleCache, file: &'static str, _hub: crate::hub::SharedHub) {
    cache.borrow_mut().insert(file, Fetched::Failed("not bundled in this build".into()));
}
