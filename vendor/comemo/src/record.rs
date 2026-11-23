//! (fork) Recording the calls made on tracked values, to check and replay them
//! later on other instances: incremental layout's checkpoints (typst-layout's
//! `incremental`). A layout records what it read between checkpoints; a later
//! layout that would start from a checkpoint checks those reads against its own
//! tracked values and, if they all give the same results, replays them into its
//! sinks (so memoized callers and the compile's own constraint see them, exactly
//! as if the work had been done again).

use parking_lot::Mutex;
use rustc_hash::FxHashSet;

use crate::track::{Call, Sink, Track, Tracked, TrackedMut};

/// Records the calls it sees, in order: immutable calls once each (by their
/// hash), mutable calls all.
pub struct Recorder<C>(Mutex<RecorderRepr<C>>);

struct RecorderRepr<C> {
    immutable: Vec<(C, u128)>,
    seen: FxHashSet<u128>,
    mutable: Vec<C>,
}

impl<C> Default for Recorder<C> {
    fn default() -> Self {
        Self(Mutex::new(RecorderRepr {
            immutable: Vec::new(),
            seen: FxHashSet::default(),
            mutable: Vec::new(),
        }))
    }
}

impl<C: Call> Recorder<C> {
    /// A new, empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many immutable and mutable calls it holds so far.
    pub fn marks(&self) -> (usize, usize) {
        let inner = self.0.lock();
        (inner.immutable.len(), inner.mutable.len())
    }

    /// Take the calls out.
    pub fn take(&self) -> Recorded<C> {
        let mut inner = self.0.lock();
        inner.seen.clear();
        Recorded {
            immutable: std::mem::take(&mut inner.immutable),
            mutable: std::mem::take(&mut inner.mutable),
        }
    }
}

impl<C: Call> Sink for Recorder<C> {
    type Call = C;

    fn emit(&self, call: C, ret: u128) -> bool {
        let mut inner = self.0.lock();
        if call.is_mutable() {
            inner.mutable.push(call);
            true
        } else if inner.seen.insert(crate::hash::hash(&call)) {
            inner.immutable.push((call, ret));
            true
        } else {
            false
        }
    }
}

/// Calls taken out of a [`Recorder`].
pub struct Recorded<C> {
    /// Immutable calls with their return hashes, deduplicated, in order.
    pub immutable: Vec<(C, u128)>,
    /// Mutable calls, in order.
    pub mutable: Vec<C>,
}

impl<C> Default for Recorded<C> {
    fn default() -> Self {
        Self { immutable: Vec::new(), mutable: Vec::new() }
    }
}

/// A sink that sends every call to a recorder and then on to the sink a tracked
/// value had before.
pub struct Tee<'a, C> {
    rec: &'a dyn Sink<Call = C>,
    prev: Option<&'a dyn Sink<Call = C>>,
}

impl<C: Call> Sink for Tee<'_, C> {
    type Call = C;

    fn emit(&self, call: C, ret: u128) -> bool {
        self.rec.emit(call.clone(), ret);
        match self.prev {
            Some(prev) => prev.emit(call, ret),
            None => true,
        }
    }
}

impl<'a, C> Tee<'a, C> {
    /// A tee: calls go to `rec`, then to `prev`.
    pub fn new(rec: &'a dyn Sink<Call = C>, prev: Option<&'a dyn Sink<Call = C>>) -> Self {
        Self { rec, prev }
    }
}

/// The sink a tracked value sends its calls to.
pub fn sink_of<'a, T>(tracked: &Tracked<'a, T>) -> Option<&'a dyn Sink<Call = T::Call>>
where
    T: Track + ?Sized,
{
    tracked.sink
}

/// The sink a mutably tracked value sends its calls to.
pub fn sink_of_mut<'a, T>(
    tracked: &TrackedMut<'a, T>,
) -> Option<&'a dyn Sink<Call = T::Call>>
where
    T: Track + ?Sized,
{
    tracked.sink
}

/// `tracked`, its calls sent to `sink` instead.
///
/// # Safety
/// The returned value (and every copy of it) must not be used after `sink`
/// is dropped: the caller puts the previous value back before that.
pub unsafe fn with_sink<'a, T>(
    tracked: Tracked<'a, T>,
    sink: &dyn Sink<Call = T::Call>,
) -> Tracked<'a, T>
where
    T: Track + ?Sized,
{
    let sink: &'a dyn Sink<Call = T::Call> = unsafe { std::mem::transmute(sink) };
    Tracked { value: tracked.value, sink: Some(sink), id: tracked.id }
}

/// Make `tracked` send its calls to `sink` (in place).
///
/// # Safety
/// As for [`with_sink`]: `tracked` must not be used after `sink` is dropped
/// unless its previous sink was put back.
pub unsafe fn set_sink_mut<'a, T>(
    tracked: &mut TrackedMut<'a, T>,
    sink: Option<&dyn Sink<Call = T::Call>>,
) where
    T: Track + ?Sized,
{
    let sink: Option<&'a dyn Sink<Call = T::Call>> = unsafe { std::mem::transmute(sink) };
    tracked.sink = sink;
}

/// Whether the recorded call `call` gives `ret` on the value behind `tracked`
/// (nothing is sent to its sinks).
pub fn check<T>(tracked: &Tracked<T>, call: &T::Call, ret: u128) -> bool
where
    T: Track + ?Sized,
{
    tracked.value.call(call) == ret
}

/// Whether the recorded call `call` gives `ret` on the value behind `tracked`.
pub fn check_mut<T>(tracked: &TrackedMut<T>, call: &T::Call, ret: u128) -> bool
where
    T: Track + ?Sized,
{
    tracked.value.call(call) == ret
}

/// Send a checked immutable call to the sinks of `tracked`, as if it had been
/// made.
pub fn replay<T>(tracked: &Tracked<T>, call: &T::Call, ret: u128)
where
    T: Track + ?Sized,
{
    if let Some(sink) = tracked.sink {
        sink.emit(call.clone(), ret);
    }
}

/// Make a recorded call on a mutably tracked value again (a mutable call is
/// performed, an immutable one only sent to the sinks with `ret`).
pub fn replay_mut<T>(tracked: &mut TrackedMut<T>, call: &T::Call, ret: u128)
where
    T: Track + ?Sized,
{
    if call.is_mutable() {
        tracked.value.call_mut(call);
        if let Some(sink) = tracked.sink {
            sink.emit(call.clone(), 0);
        }
    } else if let Some(sink) = tracked.sink {
        sink.emit(call.clone(), ret);
    }
}
