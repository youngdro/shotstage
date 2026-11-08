//! Browser entry point. Mount the editor into any DOM element and drive it from JavaScript:
//!
//! ```js
//! import init, { mount } from "./shotstage_app.js";
//! await init();
//! const editor = await mount("#stage", { scene: savedScene });   // element or selector
//! editor.dispatch("entity.create", { type: "mesh", primitive: { shape: "cube", size: [1, 1, 1] } });
//! editor.batch([{ command: "camera.dollyMove", params: { id: "camera_1", distance: 2, start: 0, end: 48 } }]);
//! const off = editor.on(ev => console.log(ev.type, ev.revision));
//! editor.commands();   // every command with its JSON Schema
//! editor.destroy();
//! ```
//!
//! Several editors can be mounted on one page; each has its own scene and history.
//!
//! Two large assets are not inside the program, so it starts sooner: the realistic-people data
//! (`human.bin`, 2 MB) and the Chinese font (`NotoSansSC-subset.otf`, 0.8 MB). Fetch them
//! alongside `init()` and hand them over when they arrive:
//!
//! ```js
//! fetch("assets/NotoSansSC-subset.otf").then(r => r.arrayBuffer()).then(b => provideFont(new Uint8Array(b)));
//! fetch("assets/human.bin").then(r => r.arrayBuffer()).then(b => provideHumanData(new Uint8Array(b)));
//! ```
//!
//! Until the font arrives Chinese text shows as boxes. Anything that needs a realistic person
//! before `provideHumanData` loads `setAssetUrls({ human })` (default `assets/human.bin`)
//! synchronously, so it still works, only with a pause.

use serde_json::{Value, json};
use shotstage_commands::{CommandError, Session};
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlCanvasElement};

use crate::hub::{self, Hub, SharedHub};

fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js(v: &JsValue) -> Result<Value, JsValue> {
    if v.is_undefined() || v.is_null() {
        return Ok(Value::Null);
    }
    let s: String = js_sys::JSON::stringify(v)?.into();
    serde_json::from_str(&s).map_err(|e| JsValue::from_str(&format!("not JSON-serialisable: {e}")))
}

fn err_js(e: CommandError) -> JsValue {
    let obj = js_sys::Error::new(&format!("{}: {}", e.code, e.message));
    let _ = js_sys::Reflect::set(&obj, &"code".into(), &e.code.into());
    obj.into()
}

/// The browser runs wasm on one thread; wgpu still asks callbacks to be `Send` when the
/// fragile-send-sync feature is on, so this wrapper vouches for values that never leave it.
struct SingleThread<T>(T);
// SAFETY: wasm32 without atomics has a single thread; the value is only used on it.
unsafe impl<T> Send for SingleThread<T> {}

impl<T> SingleThread<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

thread_local! {
    static HUMAN_URL: std::cell::RefCell<String> = std::cell::RefCell::new("assets/human.bin".into());
}

/// Where the editor fetches assets nobody provided yet: `{ human: "assets/human.bin" }`.
#[wasm_bindgen(js_name = setAssetUrls)]
pub fn set_asset_urls(urls: JsValue) -> Result<(), JsValue> {
    if let Some(url) = from_js(&urls)?.get("human").and_then(Value::as_str) {
        HUMAN_URL.with(|u| *u.borrow_mut() = url.to_string());
    }
    Ok(())
}

/// Hand over the realistic-people data (`human.bin`). Throws if it is not the file this build
/// expects (see `assetInfo`).
#[wasm_bindgen(js_name = provideHumanData)]
pub fn provide_human_data(bytes: &[u8]) -> Result<(), JsValue> {
    shotstage_core::human::provide_data(bytes).map_err(|e| JsValue::from(js_sys::Error::new(&e.to_string())))?;
    crate::i18n::repaint_all();
    Ok(())
}

/// Whether the realistic-people data is loaded.
#[wasm_bindgen(js_name = humanDataLoaded)]
pub fn human_data_loaded() -> bool {
    shotstage_core::human::data_loaded()
}

/// Hand over the Chinese interface font (`NotoSansSC-subset.otf`).
#[wasm_bindgen(js_name = provideFont)]
pub fn provide_font(bytes: Vec<u8>) -> Result<(), JsValue> {
    crate::i18n::provide_cjk_font(bytes).map_err(|e| JsValue::from(js_sys::Error::new(&e)))
}

/// What the separately downloaded files must be: `{ human: { bytes, fnv1a64 } }`.
#[wasm_bindgen(js_name = assetInfo)]
pub fn asset_info() -> JsValue {
    use shotstage_core::human::{DATA_HASH, DATA_LEN};
    to_js(&json!({ "human": { "bytes": DATA_LEN, "fnv1a64": DATA_HASH } }))
}

/// Last resort for people needed before `provideHumanData`: a synchronous request (the page
/// pauses until it is done; normally the file is already in the browser cache by then).
fn load_human_sync() -> Option<Vec<u8>> {
    let url = HUMAN_URL.with(|u| u.borrow().clone());
    log::info!("people asset needed before it was provided; loading {url}");
    let xhr = web_sys::XmlHttpRequest::new().ok()?;
    xhr.open_with_async("GET", &url, false).ok()?;
    // A synchronous request can't ask for an ArrayBuffer; this charset maps each byte to one
    // character (0x00..0x7F as is, 0x80..0xFF to U+F780..U+F7FF).
    xhr.override_mime_type("text/plain; charset=x-user-defined").ok()?;
    xhr.send().ok()?;
    if xhr.status().ok()? != 200 {
        return None;
    }
    let text = xhr.response_text().ok()??;
    Some(text.chars().map(|c| c as u32 as u8).collect())
}

/// One mounted editor.
#[wasm_bindgen]
pub struct Shotstage {
    hub: SharedHub,
    runner: eframe::WebRunner,
    canvas: HtmlCanvasElement,
}

/// Create an editor inside `container` (an `Element` or a CSS selector string). The editor fills
/// the container and follows its size. `options.scene` opens a saved scene document.
/// `options.toolbar` adds buttons to the top bar: `[{ id, label, labelZh?, tooltip?, tooltipZh? }]`;
/// a click sends `{ type: "toolbar", id }` to `editor.on` listeners.
#[wasm_bindgen]
pub async fn mount(container: JsValue, options: JsValue) -> Result<Shotstage, JsValue> {
    let _ = console_log_init();
    shotstage_core::human::set_loader(load_human_sync);
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let container: Element = if let Some(sel) = container.as_string() {
        document.query_selector(&sel)?.ok_or_else(|| JsValue::from_str(&format!("no element matches `{sel}`")))?
    } else {
        container.dyn_into::<Element>().map_err(|_| JsValue::from_str("container must be an Element or a selector"))?
    };
    let opts = from_js(&options)?;
    let session = match opts.get("scene") {
        Some(s) if !s.is_null() => {
            let scene =
                serde_json::from_value(s.clone()).map_err(|e| JsValue::from_str(&format!("options.scene: {e}")))?;
            Session::new(scene)
        }
        _ => Session::default(),
    };
    let canvas: HtmlCanvasElement = document.create_element("canvas")?.dyn_into()?;
    canvas.set_attribute("style", "display:block;width:100%;height:100%;outline:none;")?;
    canvas.set_attribute("data-shotstage", "")?;
    container.append_child(&canvas)?;

    let hub = Hub::new(session);
    if let Some(buttons) = opts.get("toolbar").filter(|b| !b.is_null()) {
        hub.borrow_mut().host_buttons =
            serde_json::from_value(buttons.clone()).map_err(|e| JsValue::from_str(&format!("options.toolbar: {e}")))?;
    }
    let runner = eframe::WebRunner::new();
    let app_hub = hub.clone();
    runner
        .start(
            canvas.clone(),
            eframe::WebOptions::default(),
            Box::new(move |cc| Ok(Box::new(crate::EditorApp::new(cc, app_hub)))),
        )
        .await?;
    Ok(Shotstage { hub, runner, canvas })
}

fn console_log_init() -> Result<(), log::SetLoggerError> {
    struct Console;
    impl log::Log for Console {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                web_sys_console(&format!("[shotstage] {}", r.args()));
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: Console = Console;
    log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info))
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn web_sys_console(s: &str);
}

#[wasm_bindgen]
impl Shotstage {
    /// Run one command: `dispatch("entity.create", {type: "mesh"})`. Returns the result; throws
    /// an `Error` with a `code` on failure.
    pub fn dispatch(&self, command: &str, params: JsValue) -> Result<JsValue, JsValue> {
        let req = json!({ "command": command, "params": from_js(&params)? });
        hub::dispatch(&self.hub, req).map(|o| to_js(&o.result)).map_err(err_js)
    }

    /// Run a raw request object: `{command, params, merge?}` or `{batch: [...]}`. Returns
    /// `{result, revision, changed}`.
    pub fn request(&self, request: JsValue) -> Result<JsValue, JsValue> {
        hub::dispatch(&self.hub, from_js(&request)?)
            .map(|o| to_js(&json!({ "result": o.result, "revision": o.revision, "changed": o.changed })))
            .map_err(err_js)
    }

    /// Run `[{command, params}, ...]` as one transaction (one undo step, all or nothing).
    pub fn batch(&self, steps: JsValue) -> Result<JsValue, JsValue> {
        hub::dispatch(&self.hub, json!({ "batch": from_js(&steps)? })).map(|o| to_js(&o.result)).map_err(err_js)
    }

    /// Every command with its parameter JSON Schema: the same catalog as the CLI and the MCP
    /// server. Host commands (`needs: ["gpu" | "files" | "view"]`) run through `call`.
    pub fn commands(&self) -> JsValue {
        to_js(&serde_json::to_value(self.hub.borrow().session.list()).unwrap_or(json!([])))
    }

    /// The scene document (save it and pass it back as `mount(el, {scene})` or `load`).
    pub fn scene(&self) -> JsValue {
        to_js(&serde_json::to_value(self.hub.borrow().session.scene()).unwrap_or(Value::Null))
    }

    /// Replace the scene (undoable).
    pub fn load(&self, scene: JsValue) -> Result<JsValue, JsValue> {
        self.dispatch("scene.load", to_js(&json!({ "scene": from_js(&scene)? })))
    }

    /// Current document revision; increases on every change.
    pub fn revision(&self) -> f64 {
        self.hub.borrow().session.revision() as f64
    }

    /// Subscribe to `{type: "change" | "command", revision, request}` events. Returns an id for `off`.
    pub fn on(&self, callback: js_sys::Function) -> u32 {
        self.hub.borrow_mut().subscribe(std::rc::Rc::new(move |ev: &Value| {
            let _ = callback.call1(&JsValue::NULL, &to_js(ev));
        }))
    }

    pub fn off(&self, id: u32) -> bool {
        self.hub.borrow_mut().unsubscribe(id)
    }

    /// Render the view through a camera (same params as the `render.snapshot` command:
    /// `{camera?, frame?, pass?: "clay"|"depth"|"normal", width?, height?}`). Resolves to
    /// `{mimeType, data (base64 PNG), dataUrl, width, height, camera, frame, pass}`.
    pub fn snapshot(&self, params: JsValue) -> js_sys::Promise {
        match from_js(&params) {
            Ok(Value::Null) => snapshot_promise(&self.hub, json!({})),
            Ok(v) => snapshot_promise(&self.hub, v),
            Err(e) => js_sys::Promise::reject(&e),
        }
    }

    /// Run any command of the catalog and resolve to its result: the same commands, params and
    /// results as the CLI and the MCP server. Document commands run at once; the host commands
    /// that render many pictures (`render.storyboard`, `render.sequence`) and `render.snapshot`
    /// resolve when the GPU is done. Rejects with an `Error` carrying `code`.
    ///
    /// ```js
    /// const board = await editor.call("render.storyboard", { passes: ["clay", "depth"] });
    /// open(URL.createObjectURL(new Blob([board.html], { type: "text/html" })));
    /// ```
    pub fn call(&self, command: String, params: JsValue) -> js_sys::Promise {
        use shotstage_commands::host::{SEQUENCE_COMMAND, STORYBOARD_COMMAND};
        let params = match from_js(&params) {
            Ok(Value::Null) => json!({}),
            Ok(v) => v,
            Err(e) => return js_sys::Promise::reject(&e),
        };
        match command.as_str() {
            shotstage_commands::snapshot::SNAPSHOT_COMMAND => snapshot_promise(&self.hub, params),
            STORYBOARD_COMMAND | SEQUENCE_COMMAND => {
                let hub = self.hub.clone();
                wasm_bindgen_futures::future_to_promise(async move {
                    render_many(&hub, &command, params).await.map(|v| to_js(&v))
                })
            }
            _ => match hub::dispatch(&self.hub, json!({ "command": command, "params": params })) {
                Ok(o) => js_sys::Promise::resolve(&to_js(&o.result)),
                Err(e) => js_sys::Promise::reject(&err_js(e)),
            },
        }
    }

    /// Stop the editor and remove its canvas from the container.
    pub fn destroy(&self) {
        self.runner.destroy();
        self.canvas.remove();
    }
}

/// Render one snapshot; resolves to the `render.snapshot` result plus `dataUrl`.
fn snapshot_promise(hub: &SharedHub, params: Value) -> js_sys::Promise {
    let drawn = hub.borrow_mut().draw_snapshot(params);
    let (plan, rs, rb) = match drawn {
        Ok(x) => x,
        Err(e) => return js_sys::Promise::reject(&err_js(e)),
    };
    let hub = hub.clone();
    let mut rb = Some(rb);
    hub.borrow_mut().pending_snapshots += 1;
    if let Some(ctx) = &hub.borrow().repaint {
        ctx.request_repaint();
    }
    js_sys::Promise::new(&mut |resolve, reject| {
        let (hub, plan) = (hub.clone(), plan.clone());
        let rb = rb.take().expect("promise executor runs once");
        let buffer = rb.buffer.clone();
        let rb2 = rb;
        let cell = SingleThread((hub, plan, rb2, resolve.clone(), reject.clone()));
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let (hub, plan, rb2, resolve, reject) = cell.into_inner();
            hub.borrow_mut().pending_snapshots -= 1;
            let out = r.map_err(|e| e.to_string()).and_then(|_| {
                let rgba = rb2.rgba()?;
                shotstage_render::encode_png(&rgba, rb2.width, rb2.height)
            });
            match out {
                Ok(png) => {
                    let b64 = shotstage_render::base64(&png);
                    let mut v = shotstage_commands::snapshot::result_json(&plan, b64.clone());
                    v["dataUrl"] = json!(format!("data:image/png;base64,{b64}"));
                    let _ = resolve.call1(&JsValue::NULL, &to_js(&v));
                }
                Err(e) => {
                    let _ = reject.call1(&JsValue::NULL, &js_sys::Error::new(&format!("failed: {e}")).into());
                }
            }
        });
        let _ = rs.device.poll(wgpu::PollType::Poll);
    })
}

/// One picture as PNG bytes.
async fn render_png(hub: &SharedHub, params: Value) -> Result<Vec<u8>, JsValue> {
    use base64::Engine as _;
    let v = wasm_bindgen_futures::JsFuture::from(snapshot_promise(hub, params)).await?;
    let v = from_js(&v)?;
    base64::engine::general_purpose::STANDARD
        .decode(v["data"].as_str().unwrap_or_default())
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// `render.storyboard` / `render.sequence` in the browser: the shared plan, rendered picture by
/// picture without blocking the page, finished by the shared code (data URLs, no files).
async fn render_many(hub: &SharedHub, command: &str, params: Value) -> Result<Value, JsValue> {
    use shotstage_commands::host::{self, STORYBOARD_COMMAND};
    let scene = hub.borrow().session.scene().clone();
    if command == STORYBOARD_COMMAND {
        let plan = host::storyboard_plan(&scene, params).map_err(err_js)?;
        if plan.out.is_some() {
            return Err(err_js(host::needs_files(command, "out")));
        }
        let mut pngs = Vec::with_capacity(plan.pictures.len());
        for p in &plan.pictures {
            pngs.push(render_png(hub, p.snapshot.clone()).await?);
        }
        return host::finish_storyboard(plan, &pngs).map_err(err_js);
    }
    let plan = host::sequence_plan(&scene, params, 640).map_err(err_js)?;
    let mut sink = host::SequenceSink::new(plan).map_err(err_js)?;
    for (i, pass, snap) in sink.jobs() {
        let png = render_png(hub, snap).await?;
        sink.add(i, pass, &png).map_err(err_js)?;
    }
    sink.finish().map_err(err_js)
}
