//! (fork) Incremental layout of page runs (`TYPST_INCREMENTAL=1`, prototype).
//!
//! A long page run records, while its flow lays out region after region, what
//! each region read of the introspector, the world and the sink (comemo's
//! `Recorder`, behind a `Tee` on the engine's tracked values), a hash of every
//! collected child, the highest child each region looked at, and the flow's
//! work state after each region (by child index). A later layout of a run with
//! the same key (page styles, run locator) collects its children as usual and
//! then starts from the last region whose children (up to what it looked at)
//! hash the same and whose recorded reads give the same results on its own
//! tracked values: the earlier regions' frames are taken as they were, their
//! reads replayed into the sinks (so memoized callers and the compile's
//! constraint see them), and the flow composes on from the restored work state.
//! The page marginals (header, footer, background, foreground) are recorded and
//! taken the same way, page by page.
//!
//! `TYPST_INCREMENTAL=check` lays every resumed run out in full too and
//! compares the pages (frames hashed), keeping the full layout; a mismatch is
//! reported on stderr.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use comemo::{Recorded, Recorder, Track, Tracked};
use rustc_hash::FxHashMap;
use typst_library::World;
use typst_library::engine::{Engine, Sink};
use typst_library::introspection::Introspector;
use typst_library::layout::Frame;
use typst_utils::Protected;

use crate::pages::LayoutedPage;

pub type ICall = <dyn Introspector as Track>::Call;
pub type WCall = <dyn World as Track>::Call;
pub type SCall = <Sink as Track>::Call;

/// The mode, from `TYPST_INCREMENTAL`: 0 off, 1 on, 2 check.
pub fn mode() -> u8 {
    static MODE: OnceLock<u8> = OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("TYPST_INCREMENTAL").as_deref() {
        Ok("1") => 1,
        Ok("check") => 2,
        _ => 0,
    })
}

pub fn log_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TYPST_INC_LOG").is_some())
}

/// (stage 3) Whether a layout's tail may be spliced from a log
/// (opt-in: `TYPST_INC_SPLICE=1`).
pub fn splice_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("TYPST_INC_SPLICE").as_deref() == Ok("1"))
}

/// The runs worth recording: at least this many children.
pub fn min_children() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("TYPST_INC_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(200)
    })
}

/// Logs kept per run key.
fn keep() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("TYPST_INC_KEEP").ok().and_then(|v| v.parse().ok()).unwrap_or(1)
    })
}

/// Recorders for the three tracked values a layout reads.
#[derive(Default)]
pub struct Recs {
    pub i: Recorder<ICall>,
    pub w: Recorder<WCall>,
    pub s: Recorder<SCall>,
}

/// Positions in recorded calls.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct Marks {
    i: usize,
    w: usize,
    s: usize,
    s_mut: usize,
}

impl Marks {
    /// These marks moved from `from` to `to` (both at or before them).
    pub fn rebase(self, from: Marks, to: Marks) -> Marks {
        Marks {
            i: self.i - from.i + to.i,
            w: self.w - from.w + to.w,
            s: self.s - from.s + to.s,
            s_mut: self.s_mut - from.s_mut + to.s_mut,
        }
    }
}

impl Recs {
    pub fn marks(&self) -> Marks {
        let (i, _) = self.i.marks();
        let (w, _) = self.w.marks();
        let (s, s_mut) = self.s.marks();
        Marks { i, w, s, s_mut }
    }

    pub fn take(&self) -> Calls {
        Calls { i: self.i.take(), w: self.w.take(), s: self.s.take() }
    }
}

/// Calls taken out of [`Recs`].
#[derive(Default)]
pub struct Calls {
    i: Recorded<ICall>,
    w: Recorded<WCall>,
    s: Recorded<SCall>,
}

impl Calls {
    /// The marks at the end of all the calls.
    pub fn end(&self) -> Marks {
        Marks {
            i: self.i.immutable.len(),
            w: self.w.immutable.len(),
            s: self.s.immutable.len(),
            s_mut: self.s.mutable.len(),
        }
    }

    /// Whether the calls in `from..to` give the same results on `engine`'s
    /// tracked values.
    pub fn check(&self, engine: &Engine, from: Marks, to: Marks) -> bool {
        let intr = engine.introspector.into_raw();
        self.i.immutable[from.i..to.i]
            .iter()
            .all(|(c, r)| comemo::check(&intr, c, *r))
            && self.w.immutable[from.w..to.w]
                .iter()
                .all(|(c, r)| comemo::check(&engine.world, c, *r))
            && self.s.immutable[from.s..to.s]
                .iter()
                .all(|(c, r)| comemo::check_mut(&engine.sink, c, *r))
    }

    /// (diagnostics) How many calls in `from..to` give other results, of how
    /// many, for the introspector, the world and the sink.
    pub fn diff_counts(&self, engine: &Engine, from: Marks, to: Marks) -> [(usize, usize); 3] {
        let intr = engine.introspector.into_raw();
        let i = &self.i.immutable[from.i..to.i];
        let w = &self.w.immutable[from.w..to.w];
        let s = &self.s.immutable[from.s..to.s];
        [
            (i.iter().filter(|(c, r)| !comemo::check(&intr, c, *r)).count(), i.len()),
            (w.iter().filter(|(c, r)| !comemo::check(&engine.world, c, *r)).count(), w.len()),
            (s.iter().filter(|(c, r)| !comemo::check_mut(&engine.sink, c, *r)).count(), s.len()),
        ]
    }

    /// Replays the calls up to `to` on `engine` (immutable ones sent to the
    /// sinks, mutable ones performed again).
    pub fn replay(&self, engine: &mut Engine, to: Marks) {
        self.replay_range(engine, Marks::default(), to);
    }

    /// Replays the calls in `from..to` on `engine`.
    pub fn replay_range(&self, engine: &mut Engine, from: Marks, to: Marks) {
        let intr = engine.introspector.into_raw();
        for (c, r) in &self.i.immutable[from.i..to.i] {
            comemo::replay(&intr, c, *r);
        }
        for (c, r) in &self.w.immutable[from.w..to.w] {
            comemo::replay(&engine.world, c, *r);
        }
        for (c, r) in &self.s.immutable[from.s..to.s] {
            comemo::replay_mut(&mut engine.sink, c, *r);
        }
        for c in &self.s.mutable[from.s_mut..to.s_mut] {
            comemo::replay_mut(&mut engine.sink, c, 0);
        }
    }
}

/// Runs `f` with `engine`'s introspector, world and sink teed into `recs`.
pub fn with_tees<R>(
    engine: &mut Engine,
    recs: &Recs,
    f: impl FnOnce(&mut Engine) -> R,
) -> R {
    let intr: Tracked<dyn Introspector> = engine.introspector.into_raw();
    let world = engine.world;
    let prev_s = comemo::sink_of_mut(&engine.sink);
    let ti = comemo::Tee::new(&recs.i, comemo::sink_of(&intr));
    let tw = comemo::Tee::new(&recs.w, comemo::sink_of(&world));
    let ts = comemo::Tee::new(&recs.s, prev_s);
    // SAFETY: the engine's previous values are put back before the tees drop
    // (and nothing keeps a copy of the engine's tracked values past `f`).
    unsafe {
        engine.introspector = Protected::from_raw(comemo::with_sink(intr, &ti));
        engine.world = comemo::with_sink(world, &tw);
        comemo::set_sink_mut(&mut engine.sink, Some(&ts));
    }
    let out = f(engine);
    engine.introspector = Protected::from_raw(intr);
    engine.world = world;
    unsafe {
        comemo::set_sink_mut(&mut engine.sink, prev_s);
    }
    out
}

/// What a flow region left behind, for resuming after it.
pub struct RegionLog<S> {
    pub frame: Frame,
    /// The recorded calls up to the end of this region.
    pub marks: Marks,
    /// How many children (from the start) the regions up to this one looked
    /// at; one more than the children if they saw the end.
    pub hw: usize,
    /// The flow's work state after this region, if it could be taken.
    pub snap: Option<Arc<S>>,
}

impl<S> Clone for RegionLog<S> {
    fn clone(&self) -> Self {
        Self { frame: self.frame.clone(), marks: self.marks, hw: self.hw, snap: self.snap.clone() }
    }
}

/// A root flow's log.
pub struct FlowLog<S> {
    pub hashes: Vec<u128>,
    pub regions: Vec<RegionLog<S>>,
    pub calls: Calls,
}

/// A page run's log.
pub struct RunLog<S> {
    pub flow: FlowLog<S>,
    pub pages: Vec<LayoutedPage>,
    /// Each page's marginals' calls.
    pub marg: Vec<Arc<Calls>>,
}

/// The flow's resume decision, made after collection.
pub struct FlowInc<S> {
    pub candidates: Vec<Arc<RunLog<S>>>,
    /// The log resumed from and the number of regions taken from it.
    pub resumed: Option<(Arc<RunLog<S>>, usize)>,
    /// (stage 3) The number of regions at the end spliced from a log.
    pub spliced: usize,
    pub out: Option<FlowLog<S>>,
}

impl<S> FlowInc<S> {
    /// The best log to resume from and how many of its regions to take.
    pub fn plan(&self, engine: &Engine, hashes: &[u128]) -> Option<(Arc<RunLog<S>>, usize)> {
        let mut best: Option<(Arc<RunLog<S>>, usize)> = None;
        for log in &self.candidates {
            let old = &log.flow.hashes;
            let same = old.iter().zip(hashes).take_while(|(a, b)| a == b).count();
            let fits = |hw: usize| {
                if hw > old.len() { same == old.len() && old.len() == hashes.len() } else { hw <= same }
            };
            // the most regions whose children are all the same
            let kmax = log.flow.regions.iter().take_while(|r| fits(r.hw)).count();
            if kmax <= best.as_ref().map_or(0, |b| b.1) {
                continue;
            }
            // their recorded reads, region by region
            let mut k = 0;
            let mut ok_k = 0;
            let mut from = Marks::default();
            while k < kmax {
                let r = &log.flow.regions[k];
                if !log.flow.calls.check(engine, from, r.marks) {
                    break;
                }
                from = r.marks;
                k += 1;
                if r.snap.is_some() {
                    ok_k = k;
                }
            }
            if ok_k > best.as_ref().map_or(0, |b| b.1) {
                best = Some((log.clone(), ok_k));
            }
        }
        best
    }
}

/// Run keys whose logs are kept (`TYPST_INC_KEYS`, default 2): past it the least recently used
/// key's logs go. A planner trial lays the same unit out again and again, so a few keys are
/// enough to resume each candidate from the one before it, across compiles.
fn keys() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| std::env::var("TYPST_INC_KEYS").ok().and_then(|v| v.parse().ok()).unwrap_or(2).max(1))
}

/// A run key's logs, the newest first, and when the key was last used.
pub struct Kept<S> {
    used: u64,
    logs: VecDeque<Arc<RunLog<S>>>,
}

impl<S> Default for Kept<S> {
    fn default() -> Self {
        Self { used: 0, logs: VecDeque::new() }
    }
}

type Store<S> = Mutex<(u64, FxHashMap<u128, Kept<S>>)>;

/// The process's logs by run key, the newest first.
pub fn store<S: Send + Sync + 'static>() -> &'static Store<S> {
    static STORE: OnceLock<Box<dyn std::any::Any + Send + Sync>> = OnceLock::new();
    STORE
        .get_or_init(|| Box::new(Store::<S>::default()))
        .downcast_ref::<Store<S>>()
        .expect("one snapshot type")
}

pub fn candidates<S: Send + Sync + 'static>(key: u128) -> Vec<Arc<RunLog<S>>> {
    let mut g = store::<S>().lock().unwrap();
    g.0 += 1;
    let tick = g.0;
    match g.1.get_mut(&key) {
        Some(k) => {
            k.used = tick;
            k.logs.iter().cloned().collect()
        }
        None => Vec::new(),
    }
}

pub fn put<S: Send + Sync + 'static>(key: u128, log: RunLog<S>) {
    let mut g = store::<S>().lock().unwrap();
    g.0 += 1;
    let tick = g.0;
    let k = g.1.entry(key).or_default();
    k.used = tick;
    k.logs.push_front(Arc::new(log));
    k.logs.truncate(keep());
    // (past the keys kept: the least recently used key's logs go)
    while g.1.len() > keys() {
        let Some(old) = g.1.iter().min_by_key(|(_, k)| k.used).map(|(key, _)| *key) else { break };
        g.1.remove(&old);
    }
}

/// Drops every log (`typst_layout::incremental_clear`).
pub fn clear<S: Send + Sync + 'static>() {
    store::<S>().lock().unwrap().1.clear();
}
