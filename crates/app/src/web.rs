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

use serde_json::{Value, json};
use shotstage_commands::{CommandError, Session};
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlCanvasElement};

use crate::hub::{self, EDITOR_COMMANDS, Hub, SharedHub};

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

/// One mounted editor.
#[wasm_bindgen]
pub struct Shotstage {
    hub: SharedHub,
    runner: eframe::WebRunner,
    canvas: HtmlCanvasElement,
}

/// Create an editor inside `container` (an `Element` or a CSS selector string). The editor fills
/// the container and follows its size. `options.scene` opens a saved scene document.
#[wasm_bindgen]
pub async fn mount(container: JsValue, options: JsValue) -> Result<Shotstage, JsValue> {
    let _ = console_log_init();
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

    /// Every command with its parameter JSON Schema (document and editor commands).
    pub fn commands(&self) -> JsValue {
        let mut list = serde_json::to_value(self.hub.borrow().session.list()).unwrap_or(json!([]));
        if let Some(a) = list.as_array_mut() {
            if let Some(snap) = a.iter_mut().find(|c| c["id"] == "render.snapshot") {
                snap["summary"] = json!(
                    "Render the view through a camera to a PNG. In the browser call `await editor.snapshot(params)`."
                );
            }
            for (id, summary) in EDITOR_COMMANDS {
                a.push(json!({ "id": id, "summary": summary, "mutates": false, "params": {"type": "object"} }));
            }
        }
        to_js(&list)
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
        let params = match from_js(&params) {
            Ok(Value::Null) => json!({}),
            Ok(v) => v,
            Err(e) => return js_sys::Promise::reject(&e),
        };
        let drawn = self.hub.borrow_mut().draw_snapshot(params);
        let (plan, rs, rb) = match drawn {
            Ok(x) => x,
            Err(e) => return js_sys::Promise::reject(&err_js(e)),
        };
        let hub = self.hub.clone();
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

    /// Stop the editor and remove its canvas from the container.
    pub fn destroy(&self) {
        self.runner.destroy();
        self.canvas.remove();
    }
}
