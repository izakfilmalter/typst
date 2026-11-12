use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;

/// (fork) The alive accelerators, by the ID of their tracked instance.
///
/// Upstream keeps them in a vector indexed from an offset, and `evict` moves the
/// offset past every ID handed out so far: from then on, no instance tracked before
/// the eviction has an accelerator. That suits an eviction between compilations. An
/// eviction *during* one (a host that trims the caches as it lays out, to bound its
/// memory) then leaves the long-lived instances of that compilation — the
/// introspector of a layout iteration — without one for the rest of it, and every
/// validation of a memoized result recomputes the return hash of each of its calls
/// on them: `query(selector)` hashes the whole result, so a counter displayed at
/// every one of n lines costs O(n) a display and O(n²) an iteration.
///
/// Here they are kept in a map, and `evict` drops those not used since the
/// eviction before it: an instance in use keeps its accelerator, one that was not
/// used for a whole eviction interval loses it (and gets a new, empty one if it is
/// used again). IDs are never reused and tracked instances are immutable while
/// tracked, so a kept accelerator is always right; only the memory differs.
static ACCELERATORS: RwLock<Option<FxHashMap<usize, Arc<Accelerator>>>> = RwLock::new(None);

/// The current ID of the accelerator.
static ID: AtomicUsize = AtomicUsize::new(0);

/// An individual accelerator: maps from call hashes to return hashes.
pub struct Accelerator {
    map: Mutex<FxHashMap<u128, u128>>,
    used: AtomicBool,
}

impl Accelerator {
    /// The map from call hashes to return hashes.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, FxHashMap<u128, u128>> {
        self.map.lock()
    }
}

/// Generate a new accelerator.
pub fn id() -> usize {
    // Get the next ID.
    ID.fetch_add(1, Ordering::SeqCst)
}

/// Evict the accelerators not used since the last eviction.
pub fn evict() {
    let mut accelerators = ACCELERATORS.write();
    if let Some(map) = accelerators.as_mut() {
        map.retain(|_, acc| acc.used.swap(false, Ordering::Relaxed));
    }
}

/// Get an accelerator by ID.
pub fn get(id: usize) -> Option<Arc<Accelerator>> {
    if let Some(acc) = ACCELERATORS.read().as_ref().and_then(|map| map.get(&id)) {
        acc.used.store(true, Ordering::Relaxed);
        return Some(acc.clone());
    }
    let mut accelerators = ACCELERATORS.write();
    let acc = accelerators
        .get_or_insert_with(FxHashMap::default)
        .entry(id)
        .or_insert_with(|| {
            Arc::new(Accelerator {
                map: Mutex::new(FxHashMap::default()),
                used: AtomicBool::new(true),
            })
        });
    acc.used.store(true, Ordering::Relaxed);
    Some(acc.clone())
}
