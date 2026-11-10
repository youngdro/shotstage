//! Triangulated meshes cached by their content, so drawing, picking and bounds do not rebuild
//! big (imported) meshes every frame. The key is a hash of the mesh source and modifiers, so it
//! survives scene copies (undo snapshots, the editor's scene snapshot) and stays valid while
//! only transforms change (dragging).

use std::cell::RefCell;
use std::fmt::Write;
use std::rc::Rc;

use crate::Result;
use crate::lru::Lru;
use crate::mesh::{MeshSource, Modifier, TriMesh, evaluate_mesh};

thread_local! {
    static CACHE: RefCell<Lru<TriMesh>> = RefCell::new(Lru::new(crate::lru::cache_budget(384)));
}

/// Memory a triangulated mesh takes (for the caches' budgets).
pub fn tri_bytes(t: &TriMesh) -> usize {
    t.positions.len() * 12 + t.normals.len() * 12 + t.indices.len() * 4 + t.uvs.len() * 8 + 64
}

/// FNV-1a over bytes.
struct Fnv(u64);

/// Hash formatted text without building the string.
impl std::fmt::Write for Fnv {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.bytes(s.as_bytes());
        Ok(())
    }
}

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
    fn word64(&mut self, w: u64) {
        self.word(w as u32);
        self.word((w >> 32) as u32);
    }
    fn word(&mut self, w: u32) {
        // whole words at a time: much faster on large vertex arrays
        self.0 ^= w as u64;
        self.0 = self.0.wrapping_mul(0x100_0000_01b3);
    }
}

fn hash_mesh(mesh: &crate::Mesh) -> u64 {
    let mut h = Fnv::new();
    h.word(mesh.positions.len() as u32);
    for p in &mesh.positions {
        for v in p {
            h.word(v.to_bits());
        }
    }
    h.word(mesh.faces.len() as u32);
    for f in &mesh.faces {
        h.word(f.len() as u32);
        for i in f {
            h.word(*i);
        }
    }
    // the UV map is drawn too: editing it must not show the old one
    h.word(mesh.uvs.len() as u32);
    for f in &mesh.uvs {
        h.word(f.len() as u32);
        for uv in f {
            h.word(uv[0].to_bits());
            h.word(uv[1].to_bits());
        }
    }
    h.0
}

fn key(source: &MeshSource, modifiers: &[Modifier]) -> Option<u64> {
    let mut h = Fnv::new();
    match source {
        MeshSource::Primitive { primitive } => _ = write!(h, "{primitive:?}"),
        // hashed once per version of the mesh (the hash is kept with the shared data)
        MeshSource::Data { mesh } => h.word64(mesh.content_hash(hash_mesh)),
        // a skinned mesh changes shape with its joints: not cached
        MeshSource::Skinned { .. } | MeshSource::Human { .. } => return None,
    }
    let _ = write!(h, "{modifiers:?}");
    Some(h.0)
}

/// Edges flatter than this many degrees are shaded smooth.
pub const SMOOTH_ANGLE: f32 = 40.0;

/// The mesh with its modifiers, triangulated with auto-smoothed normals, in its own space.
pub fn local_tris(source: &MeshSource, modifiers: &[Modifier]) -> Result<Rc<TriMesh>> {
    let Some(k) = key(source, modifiers) else {
        return Ok(Rc::new(evaluate_mesh(source, modifiers)?.triangulate_auto_smooth(SMOOTH_ANGLE)));
    };
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(k)) {
        return Ok(t);
    }
    let t = Rc::new(evaluate_mesh(source, modifiers)?.triangulate_auto_smooth(SMOOTH_ANGLE));
    CACHE.with(|c| c.borrow_mut().insert(k, t.clone(), tri_bytes(&t)));
    Ok(t)
}
