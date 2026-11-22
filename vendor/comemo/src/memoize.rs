use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parking_lot::RwLock;
use siphasher::sip128::{Hasher128, SipHasher13};

use crate::accelerate;
use crate::constraint::Constraint;
use crate::input::Input;
use crate::track::Call;
use crate::tree::{CallTree, InsertError};

/// The global list of eviction functions.
static EVICTORS: RwLock<Vec<fn(usize)>> = RwLock::new(Vec::new());

/// (fork) The memoized functions' caches by name: eviction function and statistics.
static NAMED: RwLock<Vec<(&'static str, fn(usize), fn() -> CacheStats)>> = RwLock::new(Vec::new());

/// (fork) One memoized function's cache: stored results, call-tree nodes, and hits and
/// misses since the last `stats`.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub entries: usize,
    pub nodes: usize,
    pub hits: usize,
    pub misses: usize,
    /// (fork) microseconds spent computing the misses since the last `stats` (inclusive:
    /// the memoized calls inside them count too)
    pub miss_us: u64,
    /// why lookups missed (with `set_explain`), most frequent first
    pub why: Vec<(String, usize)>,
}

/// Executes a function, trying to use a cached result for it.
#[allow(clippy::type_complexity)]
pub fn memoize<'a, In, Out, F>(
    cache: &'static Cache<In::Call, Out>,
    mut input: In,
    // These values must come from outside so that they have a lifetime that
    // allows them to be attached to the `input`. On the call site, they are
    // simply initialized as `&mut Default::default()`.
    (storage, constraint): &'a mut (
        In::Storage<&'a Constraint<In::Call>>,
        Constraint<In::Call>,
    ),
    enabled: bool,
    func: F,
) -> Out
where
    In: Input<'a>,
    In::Call: 'static,
    Out: Clone + Send + Sync + 'static,
    F: FnOnce(In) -> Out,
{
    // Early bypass if memoization is disabled.
    if !enabled {
        let output = func(input);

        // Ensure that the last call was a miss during testing.
        #[cfg(feature = "testing")]
        crate::testing::register_miss();

        return output;
    }

    // Compute the hash of the input's key part.
    let key = {
        let mut state = SipHasher13::new();
        input.key(&mut state);
        state.finish128().as_u128()
    };

    let nesting = NESTING.load(Ordering::Relaxed);

    // Check if there is a cached output.
    let hit = {
        let data = cache.0.read();
        data.lookup(key, &input).map(|(leaf, entry)| {
            // Replay mutations.
            for call in &entry.mutable {
                input.call_mut(call);
            }
            (leaf, entry.output.clone())
        })
    };
    if let Some((leaf, output)) = hit {
        cache.1.fetch_add(1, Ordering::Relaxed);
        if nesting {
            // (fork) the results this one was made of are in use too
            cache.touch(leaf, 0);
            register_child(cache, leaf);
        }

        #[cfg(feature = "testing")]
        crate::testing::register_hit();

        return output;
    }

    cache.2.fetch_add(1, Ordering::Relaxed);
    if EXPLAIN.load(Ordering::Relaxed) {
        let why = {
            let data = cache.0.read();
            if !data.tree.has_key(key) {
                String::from("new key")
            } else {
                match data.tree.explain(key, |c| input.call(c)) {
                    Some((depth, call)) => format!("call {} (depth {})", call.describe(), depth.min(9)),
                    None => String::from("?"),
                }
            }
        };
        *cache.3.lock().entry(why).or_insert(0) += 1;
    }

    // Attach the constraint.
    input.attach(storage, constraint);

    // Execute the function with the constraint attached (with `nesting`, in a frame
    // that collects the memoized results it is made of).
    let started = std::time::Instant::now();
    let (output, children) = if nesting {
        let frame = Frame::default();
        let saved = FRAME.with(|f| f.replace(Some(frame.clone())));
        let output = func(input);
        FRAME.with(|f| *f.borrow_mut() = saved);
        let children = std::mem::take(&mut *frame.0.lock());
        (output, children)
    } else {
        (func(input), Vec::new())
    };

    // (fork) what the result cost to compute: its life in the cache (`set_cost_keep`)
    let cost_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    cache.4.fetch_add(cost_us, Ordering::Relaxed);
    let bonus = match COST_KEEP_US.load(Ordering::Relaxed) {
        0 => 0,
        per => (cost_us / per).min(1 << 20) as usize,
    };

    // Insert the result into the cache. (fork, `set_admission`: a result whose key no
    // stored result has and that was not computed in the last few evictions is not stored,
    // only its key remembered: most results of a layout are never asked for again, and
    // storing them, their call trees and their eviction cost time and memory.)
    let mut data = cache.0.write();
    let admit_age = ADMIT_AGE.load(Ordering::Relaxed);
    if admit_age > 0 && !data.tree.has_key(key) && data.ghost.insert(key, 0).is_none() {
        drop(data);
        #[cfg(feature = "testing")]
        crate::testing::register_miss();
        return output;
    }
    if admit_age > 0 {
        data.ghost.remove(&key);
    }
    let inserted = data.insert(key, constraint, output.clone(), children, bonus);
    drop(data);
    match inserted {
        Ok(leaf) => {
            if nesting {
                register_child(cache, leaf);
            }
        }
        Err(InsertError::AlreadyExists) => {
            // A concurrent call with the same arguments may have inserted
            // a value in the meantime. That's okay.
        }
        Err(InsertError::MissingCall) => {
            // A missing call indicates a bug from a comemo user. See the
            // documentation for `InsertError::MissingCall` for more details.
            #[cfg(debug_assertions)]
            panic!("comemo: memoized function is non-deterministic");
        }
    }

    #[cfg(feature = "testing")]
    crate::testing::register_miss();

    output
}

/// Evict the global cache.
///
/// This removes all memoized results from the cache whose age is larger than or
/// equal to `max_age`. The age of a result grows by one during each eviction
/// and is reset to zero when the result produces a cache hit. Set `max_age` to
/// zero to completely clear the cache.
pub fn evict(max_age: usize) {
    for subevict in EVICTORS.read().iter() {
        subevict(max_age);
    }
    for (_, subevict, _) in NAMED.read().iter() {
        subevict(max_age);
    }

    accelerate::evict();
}

/// (fork) Evict each memoized function's cache with its own maximum age, given by
/// `policy` from the function's name (`module::path::fn`); `usize::MAX` keeps the cache
/// whole (its entries do not age). Caches without a name use `default`.
pub fn evict_with(default: usize, policy: &dyn Fn(&str) -> usize) {
    for subevict in EVICTORS.read().iter() {
        subevict(default);
    }
    for (name, subevict, _) in NAMED.read().iter() {
        let age = policy(name);
        if age != usize::MAX {
            subevict(age);
        }
    }

    accelerate::evict();
}

/// (fork) Microseconds of computation that buy a memoized result one more eviction of
/// life (0: off, every result ages alike). An eviction drops a result only once its age
/// passes `max_age` plus its cost over this: a result that took C µs to compute stays
/// C / `per` evictions longer (with evictions every T µs, `per = T / k` keeps a result
/// unused for k times what it cost to make). So a result that is expensive to make again outlives a trim made for the many cheap
/// ones (a book-wide index rebuilt on every page once the trim let it go: the Diatessaron's
/// `echo-cross()`, ~1.3 s a time). Eviction never changes an output, only what is reused.
static COST_KEEP_US: AtomicU64 = AtomicU64::new(0);

/// (fork) Admission (0: off, every result is stored). With `age` > 0 a result is stored
/// only when its key is already in the cache or was computed within the last `age`
/// evictions; the first time, only the key is remembered. Never changes an output.
static ADMIT_AGE: AtomicUsize = AtomicUsize::new(0);

/// (fork) Turn admission on (`age` evictions of memory for a computed key) or off (0).
pub fn set_admission(age: usize) {
    ADMIT_AGE.store(age, Ordering::Relaxed);
}

/// (fork) Set the cost that buys a result one more eviction of life (see `COST_KEEP_US`;
/// 0 turns it off). Results inserted from now on take it.
pub fn set_cost_keep(us_per_eviction: u64) {
    COST_KEEP_US.store(us_per_eviction, Ordering::Relaxed);
}

/// (fork) Statistics of every named cache.
pub fn stats() -> Vec<(&'static str, CacheStats)> {
    NAMED.read().iter().map(|(name, _, stats)| (*name, stats())).collect()
}

/// (fork) Register a named cache (the `memoize` macro does this).
pub fn register_named(name: &'static str, evict: fn(usize), stats: fn() -> CacheStats) {
    NAMED.write().push((name, evict, stats));
}

/// Register an eviction function in the global list.
pub fn register_evictor(evict: fn(usize)) {
    EVICTORS.write().push(evict);
}

/// A cache for a single memoized function.
/// (fork: with hit and miss counters)
pub struct Cache<C, Out>(
    LazyLock<RwLock<CacheData<C, Out>>>,
    AtomicUsize,
    AtomicUsize,
    LazyLock<parking_lot::Mutex<std::collections::HashMap<String, usize>>>,
    AtomicU64,
);

/// (fork) Record why lookups miss (`set_explain`).
static EXPLAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// (fork) Record, per cache, why lookups miss: a new key or the first tracked call whose
/// return differs (see `stats`; slow).
pub fn set_explain(on: bool) {
    EXPLAIN.store(on, Ordering::Relaxed);
}

impl<C: 'static, Out: 'static> Cache<C, Out> {
    /// Create an empty cache.
    ///
    /// It must take an initialization function because the `evict` fn
    /// pointer cannot be passed as an argument otherwise the function
    /// passed to `Lazy::new` is a closure and not a function pointer.
    pub const fn new(init: fn() -> RwLock<CacheData<C, Out>>) -> Self {
        Self(LazyLock::new(init), AtomicUsize::new(0), AtomicUsize::new(0), LazyLock::new(Default::default), AtomicU64::new(0))
    }

    /// Evict all entries whose age is larger than or equal to `max_age`.
    pub fn evict(&self, max_age: usize) {
        self.0.write().evict(max_age);
    }

    /// (fork) This cache's statistics; resets the hit and miss counters when `reset`.
    pub fn stats(&self) -> CacheStats {
        let data = self.0.read();
        let (entries, nodes) = data.tree.sizes();
        CacheStats {
            entries,
            nodes,
            hits: self.1.swap(0, Ordering::Relaxed),
            misses: self.2.swap(0, Ordering::Relaxed),
            miss_us: self.4.swap(0, Ordering::Relaxed),
            why: {
                let mut v: Vec<(String, usize)> = self.3.lock().drain().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v
            },
        }
    }
}

/// The internal data for a cache.
pub struct CacheData<C, Out> {
    /// Maps from hashes to memoized results.
    tree: CallTree<C, CacheEntry<C, Out>>,
    /// (fork, `set_admission`) keys computed but not stored, with their age in evictions
    ghost: rustc_hash::FxHashMap<u128, usize>,
}

/// A memoized result.
struct CacheEntry<C, Out> {
    /// The memoized function's output.
    output: Out,
    /// Mutable tracked calls that must be replayed.
    mutable: Vec<C>,
    /// How many evictions have passed since the entry has last been used.
    age: AtomicUsize,
    /// (fork, `set_nesting`) the memoized results this one was made of
    children: Vec<Child>,
    /// (fork) the eviction epoch in which it was last touched
    stamp: AtomicUsize,
    /// (fork) evictions of life beyond `max_age` its cost bought (`set_cost_keep`)
    bonus: usize,
}

impl<C, Out: 'static> CacheData<C, Out> {
    /// Evict all entries whose age is larger than or equal to `max_age`.
    fn evict(&mut self, max_age: usize) {
        let admit_age = ADMIT_AGE.load(Ordering::Relaxed);
        if max_age == 0 || admit_age == 0 {
            self.ghost.clear();
        } else if !self.ghost.is_empty() {
            self.ghost.retain(|_, age| {
                *age += 1;
                *age <= admit_age
            });
        }
        self.tree.retain(|entry| {
            let age = entry.age.get_mut();
            *age += 1;
            // (max_age 0 still clears the cache whole)
            *age <= max_age || (max_age > 0 && *age <= max_age.saturating_add(entry.bonus))
        });
    }

    /// Look for a matching entry in the cache.
    fn lookup<'a, In>(&self, key: u128, input: &In) -> Option<(usize, &CacheEntry<C, Out>)>
    where
        C: Call,
        In: Input<'a, Call = C>,
    {
        self.tree
            .get_id(key, |c| input.call(c))
            .inspect(|(_, entry)| entry.age.store(0, Ordering::SeqCst))
    }

    /// Insert an entry into the cache.
    fn insert(
        &mut self,
        key: u128,
        constraint: &Constraint<C>,
        output: Out,
        children: Vec<Child>,
        bonus: usize,
    ) -> Result<usize, InsertError>
    where
        C: Call,
    {
        let (immutable, mutable) = constraint.take();
        self.tree.insert(
            key,
            immutable,
            CacheEntry { output, mutable, age: AtomicUsize::new(0), children, stamp: AtomicUsize::new(0), bonus },
        )
    }
}

impl<C, Out> Default for CacheData<C, Out> {
    fn default() -> Self {
        Self { tree: CallTree::new(), ghost: Default::default() }
    }
}

/// (fork) Track which memoized results each result was made of (`set_nesting`).
static NESTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// (fork) A result is touched (with what it was made of) at most once per epoch.
static EPOCH: AtomicUsize = AtomicUsize::new(1);

/// (fork) Start a new touch epoch: call it when the ages of the caches whose results are
/// touched have moved on (a hit touches its parts again only in a new epoch).
pub fn new_epoch() {
    EPOCH.fetch_add(1, Ordering::Relaxed);
}

/// (fork) With nesting on, each memoized result remembers the memoized results computed or
/// reused while it was computed, and a cache hit marks them used as well (their age goes
/// back to zero). Without it, the results inside a result that keeps hitting are never
/// looked up, grow old and are evicted, although the next change below that result needs
/// them. Costs a little time and memory per call; off by default.
pub fn set_nesting(on: bool) {
    NESTING.store(on, Ordering::Relaxed);
}

/// A cache entry that can be marked used: (cache, leaf id).
type Child = (&'static dyn Touch, usize);

/// (fork) A cache whose entries can be marked used, with what they were made of.
pub trait Touch: Send + Sync {
    /// Mark the entry `leaf` and its children used.
    fn touch(&self, leaf: usize, depth: usize);
}

impl<C: std::hash::Hash + Send + Sync + 'static, Out: Send + Sync + 'static> Touch for Cache<C, Out> {
    fn touch(&self, leaf: usize, depth: usize) {
        if depth > 512 {
            return;
        }
        let epoch = EPOCH.load(Ordering::Relaxed);
        let children = {
            let data = self.0.read_recursive();
            let Some(entry) = data.tree.leaf(leaf) else { return };
            entry.age.store(0, Ordering::Relaxed);
            if entry.stamp.swap(epoch, Ordering::Relaxed) == epoch {
                return;
            }
            entry.children.clone()
        };
        for (cache, leaf) in children {
            cache.touch(leaf, depth + 1);
        }
    }
}

/// (fork) The memoized results a computation was made of so far (shared with the
/// threads it fans out to: `frame` / `in_frame`).
#[derive(Clone, Default)]
pub struct Frame(std::sync::Arc<parking_lot::Mutex<Vec<Child>>>);

thread_local! {
    static FRAME: std::cell::RefCell<Option<Frame>> = const { std::cell::RefCell::new(None) };
}

fn register_child(cache: &'static dyn Touch, leaf: usize) {
    FRAME.with(|f| {
        if let Some(frame) = &*f.borrow() {
            frame.0.lock().push((cache, leaf));
        }
    });
}

/// (fork) The current thread's frame, to carry into work it runs on other threads.
pub fn frame() -> Option<Frame> {
    if !NESTING.load(Ordering::Relaxed) {
        return None;
    }
    FRAME.with(|f| f.borrow().clone())
}

/// (fork) Run `f` in `frame` (from `frame()` on the thread that spawned this work).
pub fn in_frame<R>(frame: Option<Frame>, f: impl FnOnce() -> R) -> R {
    let Some(frame) = frame else { return f() };
    let saved = FRAME.with(|c| c.replace(Some(frame)));
    let out = f();
    FRAME.with(|c| *c.borrow_mut() = saved);
    out
}
