//! A local Blender, run in the background, converts formats shotstage does not read itself
//! (FBX, .blend) to and from GLB. Set `SHOTSTAGE_BLENDER` to the Blender executable if it is
//! not in a usual place.

use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = r#"
import bpy, sys
src, dst = sys.argv[sys.argv.index('--') + 1:][:2]
if src.lower().endswith('.blend'):
    bpy.ops.wm.open_mainfile(filepath=src)
else:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    if src.lower().endswith('.fbx'):
        bpy.ops.import_scene.fbx(filepath=src)
    else:
        bpy.ops.import_scene.gltf(filepath=src)
if dst.lower().endswith('.fbx'):
    bpy.ops.export_scene.fbx(filepath=dst, path_mode='COPY', embed_textures=True)
else:
    bpy.ops.export_scene.gltf(filepath=dst, export_format='GLB')
"#;

/// The Blender executable, if one can be found.
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SHOTSTAGE_BLENDER").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    let candidates = [
        "/Applications/Blender.app/Contents/MacOS/Blender",
        "/usr/bin/blender",
        "/usr/local/bin/blender",
        "/opt/homebrew/bin/blender",
        "/snap/bin/blender",
        "C:\\Program Files\\Blender Foundation\\Blender\\blender.exe",
    ];
    if let Some(p) = candidates.iter().map(PathBuf::from).find(|p| p.exists()) {
        return Some(p);
    }
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join("blender")).find(|p| p.exists()))
}

/// Convert `bytes` (a `from` file) to a `to` file with Blender.
pub fn convert(bytes: &[u8], from: &str, to: &str) -> Result<Vec<u8>, String> {
    let blender = find().ok_or(
        "Blender was not found: install it (blender.org) or set SHOTSTAGE_BLENDER, or export a .glb from your tool",
    )?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("shotstage-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (src, dst) = (dir.join(format!("in.{from}")), dir.join(format!("out.{to}")));
    let result = (|| {
        std::fs::write(&src, bytes).map_err(|e| e.to_string())?;
        let out = Command::new(&blender)
            .args(["-b", "--factory-startup", "--python-exit-code", "1", "--python-expr", SCRIPT, "--"])
            .arg(&src)
            .arg(&dst)
            .output()
            .map_err(|e| format!("could not run Blender ({}): {e}", blender.display()))?;
        if !out.status.success() || !dst.exists() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: String =
                err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Err(format!("Blender could not convert the {from} file:\n{tail}"));
        }
        std::fs::read(&dst).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}
