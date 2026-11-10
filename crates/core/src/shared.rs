//! [`Shared`]: big scene data (mesh geometry, skins) held behind a reference count, so copying
//! a scene (an undo snapshot, a query's scratch copy, the editor's frame copy) shares it
//! instead of duplicating it. Changing it copies on write, so every copy still behaves like
//! an independent value.

use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

struct Inner<T> {
    value: T,
    /// Content hash, worked out once per version of the value (see [`Shared::content_hash`]).
    hash: OnceLock<u64>,
}

impl<T: Clone> Clone for Inner<T> {
    fn clone(&self) -> Self {
        Self { value: self.value.clone(), hash: self.hash.clone() }
    }
}

/// A value shared between scene copies, copied only when one of them changes it.
pub struct Shared<T>(Arc<Inner<T>>);

impl<T> Shared<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Inner { value, hash: OnceLock::new() }))
    }

    /// Do two handles point at the very same data (no comparison of the contents)?
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }

    /// Address of the shared data: equal for handles that share it.
    pub fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as *const u8 as usize
    }

    /// `f(value)`, worked out once and remembered until the value changes. Used for content
    /// hashes (cache keys) of big meshes, so they are not rehashed every frame.
    pub fn content_hash(&self, f: impl FnOnce(&T) -> u64) -> u64 {
        *self.0.hash.get_or_init(|| f(&self.0.value))
    }

    /// Is this the only handle (so a change would not copy)?
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }
}

impl<T: Clone> Shared<T> {
    /// The value, copied out.
    pub fn to_inner(&self) -> T {
        self.0.value.clone()
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    /// Copies the data first when another scene copy shares it.
    fn deref_mut(&mut self) -> &mut T {
        let inner = Arc::make_mut(&mut self.0);
        inner.hash = OnceLock::new();
        &mut inner.value
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: PartialEq> PartialEq for Shared<T> {
    /// Same data is equal without looking at it (the common case after a scene copy).
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other) || self.0.value == other.0.value
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.value.fmt(f)
    }
}

impl<T: Hash> Hash for Shared<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.value.hash(state)
    }
}

impl<T: Serialize> Serialize for Shared<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.value.serialize(s)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Shared<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self::new)
    }
}

impl<T: JsonSchema> JsonSchema for Shared<T> {
    fn inline_schema() -> bool {
        T::inline_schema()
    }
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_share_until_one_changes() {
        let a = Shared::new(vec![1, 2, 3]);
        let mut b = a.clone();
        assert!(Shared::ptr_eq(&a, &b));
        let h = a.content_hash(|v| v.len() as u64);
        assert_eq!(h, 3);
        b.push(4);
        assert!(!Shared::ptr_eq(&a, &b));
        assert_eq!(*a, vec![1, 2, 3]);
        assert_eq!(*b, vec![1, 2, 3, 4]);
        // the hash is worked out again for the changed copy
        assert_eq!(b.content_hash(|v| v.len() as u64), 4);
        assert_eq!(a.content_hash(|_| 99), 3);
    }

    #[test]
    fn serialises_as_the_value() {
        let a = Shared::new(vec![1u32, 2]);
        assert_eq!(serde_json::to_string(&a).unwrap(), "[1,2]");
        let b: Shared<Vec<u32>> = serde_json::from_str("[1,2]").unwrap();
        assert_eq!(a, b);
    }
}
