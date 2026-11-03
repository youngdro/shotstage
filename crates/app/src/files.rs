//! Opening and saving files: native dialogs on the desktop, the browser's file picker,
//! drag-and-drop and downloads on the web. Picked and dropped files land in an [`Inbox`] that
//! the editor imports from on its next frame.

use std::cell::RefCell;
use std::rc::Rc;

use egui::Pos2;

/// Files waiting to be imported: (file name, bytes, where they were dropped).
pub type Inbox = Rc<RefCell<Vec<(String, Vec<u8>, Option<Pos2>)>>>;

/// File extensions the editor can import.
pub const IMPORTABLE: &[&str] = &["glb", "gltf", "obj", "stl", "fbx", "blend", "png", "jpg", "jpeg", "webp"];

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
