//! Triangulated meshes cached by their content, so drawing, picking and bounds do not rebuild
//! big (imported) meshes every frame. The key is a hash of the mesh source and modifiers, so it
//! survives scene copies (undo snapshots, the editor's scene snapshot) and stays valid while
//! only transforms change (dragging).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::Result;
use crate::mesh::{MeshSource, Modifier, TriMesh, evaluate_mesh};

const MAX_ENTRIES: usize = 512;

thread_local! {
    static CACHE: RefCell<HashMap<u64, Rc<TriMesh>>> = RefCell::new(HashMap::new());
}

/// FNV-1a over bytes.
struct Fnv(u64);

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
    fn word(&mut self, w: u32) {
        // whole words at a time: much faster on large vertex arrays
        self.0 ^= w as u64;
        self.0 = self.0.wrapping_mul(0x100_0000_01b3);
    }
}

fn key(source: &MeshSource, modifiers: &[Modifier]) -> Option<u64> {
    let mut h = Fnv::new();
    match source {
        MeshSource::Primitive { primitive } => h.bytes(format!("{primitive:?}").as_bytes()),
        MeshSource::Data { mesh } => {
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
        }
        // a skinned mesh changes shape with its joints: not cached
        MeshSource::Skinned { .. } => return None,
    }
    h.bytes(format!("{modifiers:?}").as_bytes());
    Some(h.0)
}

/// The mesh with its modifiers, triangulated with flat normals, in its own space.
pub fn local_tris(source: &MeshSource, modifiers: &[Modifier]) -> Result<Rc<TriMesh>> {
    let Some(k) = key(source, modifiers) else {
        return Ok(Rc::new(evaluate_mesh(source, modifiers)?.triangulate(false)));
    };
    if let Some(t) = CACHE.with(|c| c.borrow().get(&k).cloned()) {
        return Ok(t);
    }
    let t = Rc::new(evaluate_mesh(source, modifiers)?.triangulate(false));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= MAX_ENTRIES {
            c.clear();
        }
        c.insert(k, t.clone());
    });
    Ok(t)
}
