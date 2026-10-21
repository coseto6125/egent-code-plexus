use super::engine::Scope;
use std::ops::{Index, IndexMut};
use std::rc::Rc;

/// A snapshot copies one pointer per chunk; the first write after it copies
/// one chunk of pointers and the one scope written.
const CHUNK: usize = 64;

/// Scopes or objects shared with every snapshot that holds them and copied on
/// first write, so snapshot, restore and join cost what changed rather than
/// the size of the program. Equal pointers prove equal contents.
#[derive(Clone, Default)]
pub(super) struct Frames {
    chunks: Vec<Rc<Vec<Rc<Scope>>>>,
    len: usize,
}
impl Frames {
    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn push(&mut self, scope: Scope) {
        if self.len % CHUNK == 0 {
            self.chunks.push(Rc::new(Vec::with_capacity(CHUNK)));
        }
        Rc::make_mut(self.chunks.last_mut().unwrap()).push(Rc::new(scope));
        self.len += 1;
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = &Rc<Scope>> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }
    /// Chunk pairs that differ since `state`; frames created after it are
    /// never visited.
    fn changed<'a>(
        &'a mut self,
        state: &'a Frames,
    ) -> impl Iterator<Item = (&'a mut Rc<Scope>, &'a Rc<Scope>)> {
        self.chunks
            .iter_mut()
            .zip(&state.chunks)
            .filter(|(dst, src)| {
                !Rc::ptr_eq(&**dst, &**src)
                    && dst.iter().zip(src.iter()).any(|(d, s)| !Rc::ptr_eq(d, s))
            })
            .flat_map(|(dst, src)| Rc::make_mut(dst).iter_mut().zip(src.iter()))
            .filter(|(d, s)| !Rc::ptr_eq(&**d, &**s))
    }
    /// Return every frame `state` holds to its contents there.
    pub(super) fn restore(&mut self, state: &Frames) {
        for (dst, src) in self.changed(state) {
            *dst = src.clone();
        }
    }
    /// Union `state` into these frames.
    pub(super) fn merge(&mut self, state: &Frames) {
        for (dst, src) in self.changed(state) {
            let dst = Rc::make_mut(dst);
            for (name, value) in src.iter() {
                dst.entry(name.clone()).or_default().merge(value);
            }
        }
    }
    /// Whether the frames `prefix` holds are unchanged here.
    pub(super) fn starts_with(&self, prefix: &Frames) -> bool {
        self.chunks
            .iter()
            .zip(&prefix.chunks)
            .all(|(a, b)| Rc::ptr_eq(a, b) || a.iter().zip(b.iter()).all(|(x, y)| x == y))
    }
}
impl Index<usize> for Frames {
    type Output = Scope;
    fn index(&self, i: usize) -> &Scope {
        &self.chunks[i / CHUNK][i % CHUNK]
    }
}
/// Writing through an index copies the chunk and the scope when a snapshot
/// still shares them.
impl IndexMut<usize> for Frames {
    fn index_mut(&mut self, i: usize) -> &mut Scope {
        Rc::make_mut(&mut Rc::make_mut(&mut self.chunks[i / CHUNK])[i % CHUNK])
    }
}
