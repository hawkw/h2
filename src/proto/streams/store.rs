use super::*;

use slab;

use std::ops;
use std::collections::{HashMap, hash_map};
use std::marker::PhantomData;

/// Storage for streams
#[derive(Debug)]
pub(super) struct Store<B, P>
    where P: Peer,
{
    slab: slab::Slab<Stream<B, P>>,
    ids: HashMap<StreamId, usize>,
}

/// "Pointer" to an entry in the store
pub(super) struct Ptr<'a, B: 'a, P>
    where P: Peer + 'a,
{
    key: Key,
    slab: &'a mut slab::Slab<Stream<B, P>>,
}

/// References an entry in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key(usize);

#[derive(Debug)]
pub(super) struct Queue<B, N, P>
    where P: Peer,
{
    indices: Option<store::Indices>,
    _p: PhantomData<(B, N, P)>,
}

pub(super) trait Next {
    fn next<B, P: Peer>(stream: &Stream<B, P>) -> Option<Key>;

    fn set_next<B, P: Peer>(stream: &mut Stream<B, P>, key: Option<Key>);

    fn take_next<B, P: Peer>(stream: &mut Stream<B, P>) -> Option<Key>;

    fn is_queued<B, P: Peer>(stream: &Stream<B, P>) -> bool;

    fn set_queued<B, P: Peer>(stream: &mut Stream<B, P>, val: bool);
}

/// A linked list
#[derive(Debug, Clone, Copy)]
struct Indices {
    pub head: Key,
    pub tail: Key,
}

pub(super) enum Entry<'a, B: 'a, P: Peer + 'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a, B, P>),
}

pub(super) struct OccupiedEntry<'a> {
    ids: hash_map::OccupiedEntry<'a, StreamId, usize>,
}

pub(super) struct VacantEntry<'a, B: 'a, P>
    where P: Peer + 'a,
{
    ids: hash_map::VacantEntry<'a, StreamId, usize>,
    slab: &'a mut slab::Slab<Stream<B, P>>,
}

pub(super) trait Resolve<B, P>
    where P: Peer,
{
    fn resolve(&mut self, key: Key) -> Ptr<B, P>;
}

// ===== impl Store =====

impl<B, P> Store<B, P>
    where P: Peer,
{
    pub fn new() -> Self {
        Store {
            slab: slab::Slab::new(),
            ids: HashMap::new(),
        }
    }

    pub fn find_mut(&mut self, id: &StreamId) -> Option<Ptr<B, P>> {
        if let Some(&key) = self.ids.get(id) {
            Some(Ptr {
                key: Key(key),
                slab: &mut self.slab,
            })
        } else {
            None
        }
    }

    pub fn insert(&mut self, id: StreamId, val: Stream<B, P>) -> Ptr<B, P> {
        let key = self.slab.insert(val);
        assert!(self.ids.insert(id, key).is_none());

        Ptr {
            key: Key(key),
            slab: &mut self.slab,
        }
    }

    pub fn find_entry(&mut self, id: StreamId) -> Entry<B, P> {
        use self::hash_map::Entry::*;

        match self.ids.entry(id) {
            Occupied(e) => {
                Entry::Occupied(OccupiedEntry {
                    ids: e,
                })
            }
            Vacant(e) => {
                Entry::Vacant(VacantEntry {
                    ids: e,
                    slab: &mut self.slab,
                })
            }
        }
    }

    pub fn for_each<F>(&mut self, mut f: F) -> Result<(), ConnectionError>
        where F: FnMut(Ptr<B, P>) -> Result<(), ConnectionError>,
    {
        for &key in self.ids.values() {
            f(Ptr {
                key: Key(key),
                slab: &mut self.slab,
            })?;
        }

        Ok(())
    }
}

impl<B, P> Resolve<B, P> for Store<B, P>
    where P: Peer,
{
    fn resolve(&mut self, key: Key) -> Ptr<B, P> {
        Ptr {
            key: key,
            slab: &mut self.slab,
        }
    }
}

impl<B, P> ops::Index<Key> for Store<B, P>
    where P: Peer,
{
    type Output = Stream<B, P>;

    fn index(&self, key: Key) -> &Self::Output {
        self.slab.index(key.0)
    }
}

impl<B, P> ops::IndexMut<Key> for Store<B, P>
    where P: Peer,
{
    fn index_mut(&mut self, key: Key) -> &mut Self::Output {
        self.slab.index_mut(key.0)
    }
}

// ===== impl Queue =====

impl<B, N, P> Queue<B, N, P>
    where N: Next,
          P: Peer,
{
    pub fn new() -> Self {
        Queue {
            indices: None,
            _p: PhantomData,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_none()
    }

    pub fn take(&mut self) -> Self {
        Queue {
            indices: self.indices.take(),
            _p: PhantomData,
        }
    }

    /// Queue the stream.
    ///
    /// If the stream is already contained by the list, return `false`.
    pub fn push(&mut self, stream: &mut store::Ptr<B, P>) -> bool {
        trace!("Queue::push");

        if N::is_queued(stream) {
            trace!(" -> already queued");
            return false;
        }

        N::set_queued(stream, true);

        // The next pointer shouldn't be set
        debug_assert!(N::next(stream).is_none());

        // Queue the stream
        match self.indices {
            Some(ref mut idxs) => {
                trace!(" -> existing entries");

                // Update the current tail node to point to `stream`
                let key = stream.key();
                N::set_next(&mut stream.resolve(idxs.tail), Some(key));

                // Update the tail pointer
                idxs.tail = stream.key();
            }
            None => {
                trace!(" -> first entry");
                self.indices = Some(store::Indices {
                    head: stream.key(),
                    tail: stream.key(),
                });
            }
        }

        true
    }

    pub fn pop<'a, R>(&mut self, store: &'a mut R) -> Option<store::Ptr<'a, B, P>>
        where R: Resolve<B, P>
    {
        if let Some(mut idxs) = self.indices {
            let mut stream = store.resolve(idxs.head);

            if idxs.head == idxs.tail {
                assert!(N::next(&*stream).is_none());
                self.indices = None;
            } else {
                idxs.head = N::take_next(&mut *stream).unwrap();
                self.indices = Some(idxs);
            }

            debug_assert!(N::is_queued(&*stream));
            N::set_queued(&mut *stream, false);

            return Some(stream);
        }

        None
    }
}

// ===== impl Ptr =====

impl<'a, B: 'a, P> Ptr<'a, B, P>
    where P: Peer,
{
    pub fn key(&self) -> Key {
        self.key
    }
}

impl<'a, B: 'a, P> Resolve<B, P> for Ptr<'a, B, P>
    where P: Peer,
{
    fn resolve(&mut self, key: Key) -> Ptr<B, P> {
        Ptr {
            key: key,
            slab: &mut *self.slab,
        }
    }
}

impl<'a, B: 'a, P> ops::Deref for Ptr<'a, B, P>
    where P: Peer,
{
    type Target = Stream<B, P>;

    fn deref(&self) -> &Stream<B, P> {
        &self.slab[self.key.0]
    }
}

impl<'a, B: 'a, P> ops::DerefMut for Ptr<'a, B, P>
    where P: Peer,
{
    fn deref_mut(&mut self) -> &mut Stream<B, P> {
        &mut self.slab[self.key.0]
    }
}

// ===== impl OccupiedEntry =====

impl<'a> OccupiedEntry<'a> {
    pub fn key(&self) -> Key {
        Key(*self.ids.get())
    }
}

// ===== impl VacantEntry =====

impl<'a, B, P> VacantEntry<'a, B, P>
    where P: Peer,
{
    pub fn insert(self, value: Stream<B, P>) -> Key {
        // Insert the value in the slab
        let key = self.slab.insert(value);

        // Insert the handle in the ID map
        self.ids.insert(key);

        Key(key)
    }
}
