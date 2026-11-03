//! Opening and saving files: native dialogs on the desktop, the browser's file picker,
//! drag-and-drop and downloads on the web. Picked and dropped files land in an [`Inbox`] that
//! the editor imports from on its next frame.

use std::cell::RefCell;
use std::rc::Rc;

use egui::Pos2;

/// Files waiting to be imported: (file name, bytes, where they were dropped).
pub type Inbox = Rc<RefCell<Vec<(String, Vec<u8>, Option<Pos2>)>>>;

/// File extensions the editor can import.
pub const IMPORTABLE: &[&str] = &["glb", "gltf", "obj", "stl", "fbx", "blend", "png", "jpg", "jpeg", "webp", "json"];

/// Ask for files to import.
pub fn pick(inbox: &Inbox, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ctx;
        if let Some(paths) = rfd::FileDialog::new().add_filter("Models and images", IMPORTABLE).pick_files() {
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
                rfd::AsyncFileDialog::new().add_filter("Models and images", IMPORTABLE).pick_files().await
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
