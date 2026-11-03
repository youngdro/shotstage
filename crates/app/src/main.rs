//! Native editor: `cargo run -p shotstage-app [scene.json]`.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    use shotstage_commands::Session;

    let session = match std::env::args().nth(1) {
        Some(path) => {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
            Session::new(serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {path}: {e}")))
        }
        None => Session::default(),
    };
    let hub = shotstage_app::Hub::new(session);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 860.0]).with_title("shotstage"),
        ..Default::default()
    };
    eframe::run_native("shotstage", options, Box::new(move |cc| Ok(Box::new(shotstage_app::EditorApp::new(cc, hub)))))
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // The web build is a library: call `mount(container, options)` from JavaScript.
}
