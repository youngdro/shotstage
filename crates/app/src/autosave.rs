//! Autosave: the scene is written a couple of seconds after it changes, and at once when the
//! window closes or the browser tab is hidden, so a reload, a crash or a closed tab costs at
//! most the last moment of work.
//!
//! Where it goes: a file in `~/.shotstage` on the desktop (written on a background thread, so a
//! big scene does not stall the editor); IndexedDB in the browser, which holds hundreds of
//! megabytes where `localStorage` stopped at about five (scenes with imported models or
//! pictures went over it). An autosave left in `localStorage` by an older version is still
//! read, and cleared once IndexedDB has the scene.

use std::sync::{Arc, Mutex};

use shotstage_core::Scene;

use crate::hub::SharedHub;

/// The default autosave slot (the browser page and the desktop editor use it).
pub const DEFAULT_KEY: &str = "shotstage.autosave";

/// How an editor autosaves (`mount(el, { autosaveKey, restorePrompt })` in the browser).
#[derive(Clone, Debug)]
pub struct Settings {
    /// Slot to write to; `None` turns autosave off (a host page that keeps the scene itself).
    pub key: Option<String>,
    /// Offer to bring back the autosaved scene at start (off when the host page restores it).
    pub prompt: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { key: Some(DEFAULT_KEY.into()), prompt: true }
    }
}

/// What the writes in the background report back.
#[derive(Default)]
struct Status {
    /// A write is under way (the next one waits for it).
    writing: bool,
    /// The last write failed: why. Taken by the editor to say so once.
    error: Option<String>,
    /// The last write succeeded.
    ok: bool,
}

/// Writes the scene when it is due. Owned by the editor; one per mounted editor.
pub struct Autosaver {
    settings: Settings,
    /// Revision last written (`u64::MAX` before the first frame).
    saved: Arc<Mutex<u64>>,
    /// When the last write started (editor time, seconds).
    last: f64,
    /// Seconds the last serialisation took on this thread (the browser has no other).
    cost: f64,
    status: Arc<Mutex<Status>>,
    /// The autosave found at start has been read (nothing is written before, so it cannot
    /// be overwritten before it is offered).
    read_done: Arc<Mutex<bool>>,
    /// The autosave found at start, once read.
    found: Arc<Mutex<Option<String>>>,
    warned: bool,
    #[cfg(target_arch = "wasm32")]
    _on_hide: Option<web::Listener>,
}

impl Autosaver {
    pub fn new(settings: Settings, hub: &SharedHub) -> Self {
        #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
        let mut a = Self {
            settings,
            saved: Arc::new(Mutex::new(u64::MAX)),
            last: f64::NEG_INFINITY,
            cost: 0.0,
            status: Default::default(),
            read_done: Arc::new(Mutex::new(false)),
            found: Default::default(),
            warned: false,
            #[cfg(target_arch = "wasm32")]
            _on_hide: None,
        };
        let Some(key) = a.settings.key.clone() else {
            *a.read_done.lock().unwrap() = true;
            return a;
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = hub;
            *a.found.lock().unwrap() = native::read(&key);
            *a.read_done.lock().unwrap() = true;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (found, done) = (a.found.clone(), a.read_done.clone());
            let k = key.clone();
            wasm_bindgen_futures::spawn_local(async move {
                *found.lock().unwrap() = web::read(&k).await;
                *done.lock().unwrap() = true;
            });
            a._on_hide = web::on_hide(hub.clone(), key, a.saved.clone(), a.status.clone(), a.read_done.clone());
        }
        a
    }

    /// The scene found in the autosave at start, once (after it has been read).
    pub fn take_found(&mut self) -> Option<String> {
        if !self.settings.prompt {
            return None;
        }
        self.found.lock().unwrap().take()
    }

    /// Call every frame. `hold` keeps writes back (an autosave offer is still open);
    /// `busy` puts them off a little (a drag in progress: serialising a big scene in the
    /// middle of it would stutter). Returns a warning to show once when writing fails.
    pub fn tick(&mut self, ctx: &egui::Context, hub: &SharedHub, hold: bool, busy: bool) -> Option<String> {
        let key = self.settings.key.clone()?;
        let now = ctx.input(|i| i.time);
        let warning = self.take_error();
        if hold || !*self.read_done.lock().unwrap() {
            return warning;
        }
        let rev = hub.borrow().session.revision();
        {
            let mut saved = self.saved.lock().unwrap();
            if *saved == u64::MAX {
                // first frame: remember where we start; save after the next change
                *saved = rev;
                return warning;
            }
            if *saved == rev {
                return warning;
            }
        }
        // a big scene takes a while to turn into text: write it less often
        let every = (self.cost * 40.0).clamp(2.0, 15.0);
        let since = now - self.last;
        let wait = if busy && since < 10.0 { (10.0 - since).min(0.5) } else { every - since };
        if wait > 0.0 || self.status.lock().unwrap().writing {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait.max(0.1) + 0.02));
            return warning;
        }
        let scene = hub.borrow().session.scene().clone();
        self.last = now;
        *self.saved.lock().unwrap() = rev;
        self.cost = write(&key, scene, &self.status, false);
        warning
    }

    /// Write now if anything is unsaved (the window is closing).
    pub fn flush(&mut self, hub: &SharedHub) {
        let Some(key) = self.settings.key.clone() else { return };
        if !*self.read_done.lock().unwrap() {
            return;
        }
        let Ok(h) = hub.try_borrow() else { return };
        let rev = h.session.revision();
        let mut saved = self.saved.lock().unwrap();
        if *saved == rev || *saved == u64::MAX {
            return;
        }
        *saved = rev;
        let scene = h.session.scene().clone();
        drop(saved);
        write(&key, scene, &self.status, true);
    }

    fn take_error(&mut self) -> Option<String> {
        let mut s = self.status.lock().unwrap();
        if s.ok {
            s.ok = false;
            self.warned = false;
        }
        let e = s.error.take()?;
        log::warn!("autosave: {e}");
        // say it once, not every two seconds
        (!std::mem::replace(&mut self.warned, true)).then_some(e)
    }
}

/// Start writing `scene` to `key`; returns the seconds spent on this thread.
fn write(key: &str, scene: Scene, status: &Arc<Mutex<Status>>, wait: bool) -> f64 {
    status.lock().unwrap().writing = true;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (key, status) = (key.to_string(), status.clone());
        let job = move || {
            let r = serde_json::to_string(&scene).map_err(|e| e.to_string()).and_then(|j| native::write(&key, &j));
            finish(&status, r);
        };
        if wait {
            job();
        } else {
            std::thread::spawn(job);
        }
        0.0
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = wait;
        let t0 = js_sys::Date::now();
        let json = serde_json::to_string(&scene).unwrap_or_default();
        let cost = (js_sys::Date::now() - t0) / 1000.0;
        let (key, status) = (key.to_string(), status.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let r = web::write(&key, &json).await;
            finish(&status, r);
        });
        cost
    }
}

fn finish(status: &Arc<Mutex<Status>>, r: Result<(), String>) {
    let mut s = status.lock().unwrap();
    s.writing = false;
    match r {
        Ok(()) => s.ok = true,
        Err(e) => s.error = Some(e),
    }
}

/// The autosaved scene in `key` (for the browser page's own restore).
#[cfg(target_arch = "wasm32")]
pub async fn read(key: &str) -> Option<String> {
    web::read(key).await
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::{Path, PathBuf};

    fn dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?).join(".shotstage"))
    }

    fn file(dir: &Path, key: &str) -> PathBuf {
        let name = if key == super::DEFAULT_KEY {
            "autosave".to_string()
        } else {
            key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
        };
        dir.join(format!("{name}.json"))
    }

    pub fn read(key: &str) -> Option<String> {
        read_in(&dir()?, key)
    }

    pub fn write(key: &str, json: &str) -> Result<(), String> {
        write_in(&dir().ok_or("no home folder")?, key, json)
    }

    fn read_in(dir: &Path, key: &str) -> Option<String> {
        std::fs::read_to_string(file(dir, key)).ok()
    }

    fn write_in(dir: &Path, key: &str, json: &str) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let p = file(dir, key);
        // a temporary file per write, renamed over the old one: a crash mid-write leaves the
        // previous autosave whole, and two writes never share a temporary file
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = p.with_extension(format!("json.{}.{n}.tmp", std::process::id()));
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn writes_replace_the_file_whole() {
            let dir = std::env::temp_dir().join(format!("shotstage-autosave-{}", std::process::id()));
            super::write_in(&dir, "test.slot", "{\"a\":1}").unwrap();
            super::write_in(&dir, "test.slot", "{\"a\":2}").unwrap();
            assert_eq!(super::read_in(&dir, "test.slot").as_deref(), Some("{\"a\":2}"));
            let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
            assert_eq!(left.len(), 1, "no temporary files left behind");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use js_sys::Promise;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

    use super::{Status, finish};
    use crate::hub::SharedHub;

    const DB: &str = "shotstage";
    const STORE: &str = "autosave";

    thread_local! {
        static OPEN: RefCell<Option<IdbDatabase>> = const { RefCell::new(None) };
    }

    fn js_err(e: JsValue) -> String {
        if let Some(d) = e.dyn_ref::<web_sys::DomException>() {
            if d.name() == "QuotaExceededError" {
                return "the browser has no room left for it".into();
            }
            return d.message();
        }
        e.as_string().unwrap_or_else(|| format!("{e:?}"))
    }

    /// Resolve with the request's result when it succeeds.
    fn done(req: &IdbRequest) -> Promise {
        let req = req.clone();
        Promise::new(&mut |resolve, reject| {
            let r = req.clone();
            let ok = Closure::once_into_js(move |_: web_sys::Event| {
                let _ = resolve.call1(&JsValue::NULL, &r.result().unwrap_or(JsValue::UNDEFINED));
            });
            let r = req.clone();
            let fail = Closure::once_into_js(move |_: web_sys::Event| {
                let e = r.error().ok().flatten().map(JsValue::from).unwrap_or_else(|| "request failed".into());
                let _ = reject.call1(&JsValue::NULL, &e);
            });
            req.set_onsuccess(Some(ok.unchecked_ref()));
            req.set_onerror(Some(fail.unchecked_ref()));
        })
    }

    async fn db() -> Result<IdbDatabase, String> {
        if let Some(d) = OPEN.with(|o| o.borrow().clone()) {
            return Ok(d);
        }
        let factory = web_sys::window()
            .and_then(|w| w.indexed_db().ok().flatten())
            .ok_or("this browser keeps no storage for the page")?;
        let req: IdbOpenDbRequest = factory.open_with_u32(DB, 1).map_err(js_err)?;
        let upgrade = Closure::once_into_js(move |e: web_sys::Event| {
            let db = e
                .target()
                .and_then(|t| t.dyn_into::<IdbRequest>().ok())
                .and_then(|r| r.result().ok())
                .and_then(|r| r.dyn_into::<IdbDatabase>().ok());
            if let Some(db) = db
                && !db.object_store_names().contains(STORE)
            {
                let _ = db.create_object_store(STORE);
            }
        });
        req.set_onupgradeneeded(Some(upgrade.unchecked_ref()));
        let db: IdbDatabase = JsFuture::from(done(&req)).await.map_err(js_err)?.dyn_into().map_err(js_err)?;
        OPEN.with(|o| *o.borrow_mut() = Some(db.clone()));
        Ok(db)
    }

    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub async fn read(key: &str) -> Option<String> {
        let from_idb = async {
            let db = db().await.ok()?;
            let store = db.transaction_with_str(STORE).ok()?.object_store(STORE).ok()?;
            let v = JsFuture::from(done(&store.get(&key.into()).ok()?)).await.ok()?;
            v.as_string()
        };
        match from_idb.await {
            Some(s) => Some(s),
            // written by an older version
            None => local_storage()?.get_item(key).ok()?,
        }
    }

    pub async fn write(key: &str, json: &str) -> Result<(), String> {
        let db = db().await?;
        let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(js_err)?;
        let store = tx.object_store(STORE).map_err(js_err)?;
        let req = store.put_with_key(&json.into(), &key.into()).map_err(js_err)?;
        JsFuture::from(done(&req)).await.map_err(js_err)?;
        // it is in IndexedDB now: free the old, small storage
        if let Some(ls) = local_storage() {
            let _ = ls.remove_item(key);
        }
        Ok(())
    }

    /// Keeps the page-hide listener registered while the editor lives.
    pub struct Listener {
        target: web_sys::Document,
        callback: Closure<dyn FnMut()>,
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let f = self.callback.as_ref().unchecked_ref();
            let _ = self.target.remove_event_listener_with_callback("visibilitychange", f);
            if let Some(w) = web_sys::window() {
                let _ = w.remove_event_listener_with_callback("pagehide", f);
            }
        }
    }

    /// Write at once when the tab is hidden or the page goes away: frames stop then, so the
    /// timed save would not come, and a tab closed right after an edit would lose it.
    pub fn on_hide(
        hub: SharedHub,
        key: String,
        saved: Arc<Mutex<u64>>,
        status: Arc<Mutex<Status>>,
        read_done: Arc<Mutex<bool>>,
    ) -> Option<Listener> {
        let window = web_sys::window()?;
        let document = window.document()?;
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            if doc.visibility_state() != web_sys::VisibilityState::Hidden || !*read_done.lock().unwrap() {
                return;
            }
            let Ok(h) = hub.try_borrow() else { return };
            let rev = h.session.revision();
            {
                let mut s = saved.lock().unwrap();
                if *s == rev || *s == u64::MAX {
                    return;
                }
                *s = rev;
            }
            let Ok(json) = serde_json::to_string(h.session.scene()) else { return };
            drop(h);
            status.lock().unwrap().writing = true;
            let (key, status) = (key.clone(), status.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let r = write(&key, &json).await;
                finish(&status, r);
            });
        });
        let f = callback.as_ref().unchecked_ref();
        document.add_event_listener_with_callback("visibilitychange", f).ok()?;
        let _ = window.add_event_listener_with_callback("pagehide", f);
        Some(Listener { target: document, callback })
    }
}
