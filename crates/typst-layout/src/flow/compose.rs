use std::num::NonZeroUsize;

use typst_library::diag::{SourceResult, bail};
use typst_library::engine::Engine;
use typst_library::foundations::{
    Content, NativeElement, Packed, Resolve, ShowSet, Smart,
};
use typst_library::introspection::{
    Counter, CounterDisplayElem, CounterState, CounterUpdate, Location, Locator,
    SplitLocator, Tag,
};
use typst_library::layout::{
    Abs, Axes, Dir, FixedAlignment, Fr, Fragment, Frame, FrameItem, FrameParent, Inherit,
    OuterHAlignment, PlacementScope, Point, Region, Regions, Rel, Size,
};
use typst_library::model::{
    FootnoteElem, FootnoteEntry, LineNumberingScope, Numbering, ParLineMarker,
};
use typst_library::pdf::ArtifactKind;
use typst_syntax::Span;
use typst_utils::{NonZeroExt, Numeric};

use super::{
    BalanceSpec, Config, FlowMode, FlowResult, FootnoteSpill, LaneWidth, LineNumberConfig,
    PlacedChild, Stop, Work, distribute,
};

/// Composes the contents of a single page/region. A region can have multiple
/// columns/subregions.
///
/// The composer is primarily concerned with layout of out-of-flow insertions
/// (floats and footnotes).  It does this in per-page and per-column loops that
/// rerun when a new float is added (since it affects the regions available to
/// the distributor).
///
/// To lay out the in-flow contents of individual subregions, the composer
/// invokes [distribution](distribute()).
pub fn compose(
    engine: &mut Engine,
    work: &mut Work,
    config: &Config,
    locator: Locator,
    regions: Regions,
) -> SourceResult<Frame> {
    Composer {
        engine,
        config,
        page_base: regions.base(),
        column: 0,
        page_insertions: Insertions::default(),
        column_insertions: Insertions::default(),
        work,
        footnote_spill: vec![],
        footnote_queue: vec![],
        deferred: vec![],
        cuts: vec![],
        uncuttable: vec![],
        balance: None,
        pending: 0,
        flow_used: Abs::zero(),
        used: Abs::zero(),
        extents: vec![],
        reserves: vec![],
    }
    .page(locator, regions)
}

/// State for composition.
///
/// Sadly, we need that many lifetimes because &mut references are invariant and
/// it would force the lifetimes of various things to be equal if they
/// shared a lifetime.
///
/// The only interesting lifetimes are 'a and 'b. See [Work] for more details
/// about them.
pub struct Composer<'a, 'b, 'x, 'y> {
    pub engine: &'x mut Engine<'y>,
    pub work: &'x mut Work<'a, 'b>,
    pub config: &'x Config<'x>,
    /// The index of the column being laid out.
    pub column: usize,
    page_base: Size,
    page_insertions: Insertions<'a, 'b>,
    column_insertions: Insertions<'a, 'b>,
    // These are here because they have to survive relayout (we could lose the
    // footnotes otherwise). For floats, we revisit them anyway, so it's okay to
    // use `work.floats` directly. This is not super clean; probably there's a
    // better way.
    footnote_spill: Vec<FootnoteSpill>,
    footnote_queue: Vec<Packed<FootnoteElem>>,
    /// Footnotes of the current column whose entries were laid out although
    /// their markers ended up in a later region (e.g. because orphan
    /// prevention or a sticky block moved the marker's frame after the entry
    /// was reserved). When the column is laid out again, their markers'
    /// frames move on instead of reserving the entries again.
    deferred: Vec<Location>,
    /// Height limits for the first frame of breakable blocks in the current
    /// column, by block id. They make a block break above the line of a
    /// marker whose entry didn't fit below it.
    pub cuts: Vec<(usize, Abs)>,
    /// Footnotes of the current column whose breakable block can't usefully
    /// be cut above the marker.
    pub uncuttable: Vec<Location>,
    /// Set by the distributor when it reaches a column balance marker, with
    /// the marker's height step. The columns after the marker's column stay
    /// empty, and [`Self::page`] then balances the columns.
    pub balance: Option<BalanceSpec>,
    /// The number of insertions (queued floats and footnotes, split footnote
    /// entries) that were pending at the ends of the region's columns, summed
    /// over the columns. Used to reject balanced layouts that displace
    /// insertions more than the unbalanced layout.
    pending: usize,
    /// The height used by in-flow content in the last distributed column,
    /// set by the distributor.
    pub flow_used: Abs,
    /// The height used by in-flow content and column insertions, summed
    /// over the region's columns. Gives a lower bound for balancing.
    used: Abs,
    /// Per column of the region laid out last: where its in-flow content
    /// ends (below its top insertions) and the height of its bottom
    /// insertions (floats, footnote separator and entries).
    extents: Vec<(Abs, Abs)>,
    /// In a balanced layout with level notes: space reserved at the foot of
    /// each column (by index) below its bottom insertions, so that they start
    /// where the deepest column's do.
    reserves: Vec<Abs>,
}

/// How many column heights balancing tries upwards from its lower bound
/// before it falls back to bisection.
const SCAN_TRIALS: usize = 16;

/// How a footnote's entry is processed.
#[derive(Debug, Copy, Clone)]
struct NoteCtx {
    /// Space used by in-flow content up to (and possibly including) the
    /// marker. See [`Composer::footnotes`].
    flow_need: Abs,
    /// Whether the marker's frame may migrate if the entry doesn't fit.
    migratable: bool,
    /// Whether the marker is in an atomic in-flow frame of the distributor,
    /// which can always migrate as a whole by finishing the region.
    atomic: bool,
    /// Whether the marker is in a frame of a breakable block that can break
    /// above the marker, see [`Stop::Cut`].
    cuttable: bool,
    /// Whether the marker is in this column's flow (rather than in an earlier
    /// region or another entry), so it must end up in this column.
    fresh: bool,
}

impl<'a, 'b> Composer<'a, 'b, '_, '_> {
    /// Lay out a container/page region, including container/page insertions.
    fn page(mut self, locator: Locator, regions: Regions) -> SourceResult<Frame> {
        // This loop can restart region layout when requested to do so by a
        // `Stop`. This happens when there is a parent-scoped float.
        let checkpoint = self.work.clone();
        let initial = (self.footnote_spill.clone(), self.footnote_queue.clone());
        let (output, pod) = loop {
            // Shrink the available space by the space used by page
            // insertions.
            let mut pod = regions;
            pod.size.y -= self.page_insertions.height();

            self.balance = None;
            self.pending = 0;
            self.used = Abs::zero();
            match self.page_contents(locator.relayout(), pod, None) {
                Ok(frame) => break (frame, pod),
                Err(Stop::Finish(_)) => unreachable!(),
                Err(Stop::Relayout(PlacementScope::Column)) => unreachable!(),
                Err(Stop::Cut(..)) => unreachable!(),
                Err(Stop::Relayout(PlacementScope::Parent)) => {
                    *self.work = checkpoint.clone();
                    continue;
                }
                Err(Stop::Error(err)) => return Err(err),
            };
        };

        // Balance the columns if the region ended at a balance marker.
        let output = match self.balance.take() {
            Some(spec) if self.config.columns.count > 1 => {
                self.balance(locator, pod, &checkpoint, initial, output, spec)?
            }
            _ => output,
        };
        drop(checkpoint);

        Ok(self.page_insertions.finalize(self.work, self.config, output))
    }

    /// Balances the columns of a region whose (unbalanced) layout `normal`
    /// ended at a balance marker: searches for the smallest column height at
    /// which the region's contents up to the marker still fit into its
    /// columns, with the same insertions as in the unbalanced layout. Page
    /// insertions (parent-scoped floats) stay as they are.
    ///
    /// Heights are multiples of `snap` if given, otherwise of 1pt. If no
    /// height shorter than the full column height fits, the unbalanced layout
    /// is kept.
    fn balance(
        &mut self,
        locator: Locator,
        pod: Regions,
        checkpoint: &Work<'a, 'b>,
        initial: (Vec<FootnoteSpill>, Vec<Packed<FootnoteElem>>),
        normal: Frame,
        spec: BalanceSpec,
    ) -> SourceResult<Frame> {
        let full = pod.size.y;
        let snap = spec.snap.filter(|snap| *snap > Abs::zero());
        if !full.is_finite() || full <= Abs::zero() {
            return Ok(normal);
        }

        let budget = self.pending;
        let normal_work = self.work.clone();
        let page_insertions = self.page_insertions.clone();

        // Candidate heights are `offset + step * k` for `k` in `1..=max`;
        // `max + 1` stands for the unbalanced layout, which is known to fit.
        let step = snap.unwrap_or(Abs::pt(1.0));
        let offset = if snap.is_some() {
            let k = (spec.offset / step).floor();
            spec.offset - step * k
        } else {
            Abs::zero()
        };
        let height = |k: usize| offset + step * k as f64;
        let max = ((full - offset) / step + 1e-6).floor() as usize;
        let max = if height(max).approx_eq(full) { max.saturating_sub(1) } else { max };

        // Whether content fits is not monotonic in the height: e.g. a
        // footnote entry that doesn't fit at all lets its marker's line move
        // to the next column, while a slightly taller column would split the
        // entry. So instead of bisecting right away, we scan upwards from a
        // lower bound, the region's content shared evenly between the
        // columns (minus a margin, as spacing collapses at column breaks).
        let count = self.config.columns.count as f64;
        let margin = ((Abs::pt(4.0) / step).ceil() as usize).max(2);
        let start = (((self.used / count - offset) / step).floor().max(0.0) as usize)
            .saturating_sub(margin)
            .clamp(1, max + 1);
        let end = (start + SCAN_TRIALS).min(max + 1);

        let mut best = None;
        for k in start..end {
            let trial = self.balance_trial(
                &locator,
                pod,
                checkpoint,
                &initial,
                height(k),
                budget,
                spec.level_notes,
                step,
            )?;
            if trial.is_some() {
                best = trial;
                break;
            }
        }

        // If the scan didn't find a fitting height, bisect the rest.
        if best.is_none() {
            let (mut lo, mut hi) = (end.saturating_sub(1), max + 1);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                match self.balance_trial(
                    &locator,
                    pod,
                    checkpoint,
                    &initial,
                    height(mid),
                    budget,
                    spec.level_notes,
                    step,
                )? {
                    Some(result) => {
                        hi = mid;
                        best = Some(result);
                    }
                    None => lo = mid,
                }
            }
        }

        // Level notes that cannot be had at any height (a short column with
        // many notes beside a long one with few): the columns at full height,
        // unlevelled, each column's notes still raised to right under its text.
        if best.is_none() && spec.level_notes {
            best = self.balance_trial(
                &locator, pod, checkpoint, &initial, full, budget, false, step,
            )?;
        }

        self.page_insertions = page_insertions;
        self.balance = None;
        Ok(match best {
            Some((frame, work)) => {
                *self.work = work;
                frame
            }
            None => {
                *self.work = normal_work;
                normal
            }
        })
    }

    /// Lays out the region's columns with the given `height`. Returns the
    /// frame and the resulting work if the balance marker was reached in this
    /// region without displacing more insertions than the `budget`.
    fn balance_trial(
        &mut self,
        locator: &Locator,
        pod: Regions,
        checkpoint: &Work<'a, 'b>,
        initial: &(Vec<FootnoteSpill>, Vec<Packed<FootnoteElem>>),
        height: Abs,
        budget: usize,
        level_notes: bool,
        step: Abs,
    ) -> SourceResult<Option<(Frame, Work<'a, 'b>)>> {
        let page_insertions = self.page_insertions.clone();
        // Level notes: each column with notes keeps free, below them, the
        // difference to the deepest column's notes, so that every column's
        // notes start at the same height and its flow ends above it. Laying
        // out with those reserves can move content between columns, so repeat
        // until they are stable (or give up on this height).
        let mut reserves: Vec<Abs> = vec![];
        let mut rounds = 0;
        let result = loop {
            *self.work = checkpoint.clone();
            self.footnote_spill = initial.0.clone();
            self.footnote_queue = initial.1.clone();
            self.balance = None;
            self.pending = 0;
            self.used = Abs::zero();
            self.reserves = reserves.clone();

            let result = self.page_contents(locator.relayout(), pod, Some(height));
            self.reserves.clear();
            if !level_notes || result.is_err() {
                break result;
            }
            let deepest = self.extents.iter().map(|e| e.1).fold(Abs::zero(), Abs::max);
            let want: Vec<Abs> = self
                .extents
                .iter()
                .map(|e| if e.1 > Abs::zero() { deepest - e.1 } else { Abs::zero() })
                .collect();
            let same = want.len() == reserves.len()
                && want.iter().zip(&reserves).all(|(a, b)| a.approx_eq(*b))
                || (reserves.is_empty() && want.iter().all(|w| w.approx_empty()));
            if same {
                break result;
            }
            rounds += 1;
            if rounds > 4 {
                self.balance = None;
                self.page_insertions = page_insertions;
                self.footnote_spill.clear();
                self.footnote_queue.clear();
                return Ok(None);
            }
            reserves = want;
        };
        let reached = self.balance.take().is_some();
        let fits = reached && self.pending <= budget;
        // Notes stand right under the text: the balanced height is set by the
        // deepest column, so a column whose flow ends higher would leave its
        // notes at the foot, with a gap above their separator. Raise them by
        // the whole steps of that gap (so they stay on the step's grid) — with
        // level notes by the same amount in every column that has notes (the
        // smallest gap), so their separators stay level. The flow keeps its
        // breaks: each column still has room for what it holds.
        let mut result = result;
        if fits && result.is_ok() && step > Abs::zero() {
            let gaps: Vec<Option<Abs>> = self
                .extents
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    (e.1 > Abs::zero()).then(|| {
                        height - reserves.get(i).copied().unwrap_or_default() - e.1 - e.0
                    })
                })
                .collect();
            let lift_of = |g: Abs| step * ((g / step + 1e-6).floor().max(0.0));
            // Notes follow their own text: level separators only where the
            // columns' text ends within a step of each other; a column whose text
            // ends well above the other's raises its notes under its own text.
            let own: Vec<Abs> = gaps.iter().map(|g| g.map(lift_of).unwrap_or_default()).collect();
            let lifts: Vec<Abs> = if level_notes {
                let lo = gaps.iter().flatten().copied().reduce(Abs::min).map(lift_of);
                let hi = gaps.iter().flatten().copied().reduce(Abs::max).map(lift_of);
                match (lo, hi) {
                    (Some(lo), Some(hi)) if hi - lo <= step + Abs::pt(0.01) => {
                        gaps.iter().map(|g| if g.is_some() { lo } else { Abs::zero() }).collect()
                    }
                    _ => own,
                }
            } else {
                own
            };
            if lifts.iter().any(|l| *l > Abs::zero()) {
                let flows: Vec<Abs> = self.extents.iter().map(|e| e.0).collect();
                let work = self.work.clone();
                let spill = self.footnote_spill.clone();
                let queue = self.footnote_queue.clone();
                *self.work = checkpoint.clone();
                self.footnote_spill = initial.0.clone();
                self.footnote_queue = initial.1.clone();
                self.balance = None;
                self.pending = 0;
                self.used = Abs::zero();
                self.reserves = (0..lifts.len())
                    .map(|i| reserves.get(i).copied().unwrap_or_default() + lifts[i])
                    .collect();
                let lifted = self.page_contents(locator.relayout(), pod, Some(height));
                self.reserves.clear();
                let again = self.balance.take().is_some() && self.pending <= budget;
                let same = self.extents.len() == flows.len()
                    && self.extents.iter().zip(&flows).all(|(e, f)| e.0.approx_eq(*f));
                match lifted {
                    Ok(frame) if again && same => result = Ok(frame),
                    Err(Stop::Error(err)) => return Err(err),
                    _ => {
                        // (the lifted layout differs: keep the one found)
                        *self.work = work;
                        self.footnote_spill = spill;
                        self.footnote_queue = queue;
                    }
                }
            }
        }
        self.page_insertions = page_insertions;
        match result {
            Ok(frame) if fits => Ok(Some((frame, self.work.clone()))),
            Ok(_) | Err(Stop::Relayout(_) | Stop::Finish(_) | Stop::Cut(..)) => {
                self.footnote_spill.clear();
                self.footnote_queue.clear();
                Ok(None)
            }
            Err(Stop::Error(err)) => Err(err),
        }
    }

    /// Lay out the inner contents of a container/page.
    ///
    /// With a `balance` height, the region's columns get that height and
    /// later regions get at least the full height, so that contents that
    /// don't fit move on instead of overflowing.
    fn page_contents(
        &mut self,
        locator: Locator,
        regions: Regions,
        balance: Option<Abs>,
    ) -> FlowResult<Frame> {
        // No point in create column regions, if there's just one!
        if self.config.columns.count == 1 {
            return self.column(locator, regions);
        }

        // Create a backlog for multi-column layout.
        self.extents.clear();
        let count = self.config.columns.count;
        let column_height = regions.size.y;
        let first = balance.unwrap_or(column_height);
        let backlog: Vec<_> = match balance {
            None => std::iter::once(&column_height)
                .chain(regions.backlog)
                .flat_map(|&h| std::iter::repeat_n(h, count))
                .skip(1)
                .collect(),
            Some(_) => std::iter::repeat_n(first, count - 1)
                .chain(
                    std::iter::once(&column_height)
                        .chain(regions.backlog)
                        .flat_map(|&h| std::iter::repeat_n(h, count)),
                )
                .collect(),
        };

        // Subregions for column layout.
        let mut inner = Regions {
            size: Size::new(self.config.columns.width, first),
            backlog: &backlog,
            expand: Axes::new(true, regions.expand.y),
            ..regions
        };

        // The size of the merged frame hosting multiple columns.
        let size = Size::new(
            regions.size.x,
            if regions.expand.y { regions.size.y } else { Abs::zero() },
        );

        let mut output = Frame::hard(size);
        let mut offset = Abs::zero();
        let mut locator = locator.split();

        // Lay out the columns and stitch them together.
        for i in 0..count {
            self.column = i;
            let locator = locator.next(&());

            // Columns after a balance marker stay empty.
            let frame = if self.balance.is_some() {
                let height = if regions.expand.y { inner.size.y } else { Abs::zero() };
                Frame::soft(Size::new(self.config.columns.width, height))
            } else {
                let frame = self.column(locator, inner)?;
                self.pending += self.work.floats.len()
                    + self.work.footnotes.len()
                    + self.work.footnote_spill.len();
                frame
            };

            if !regions.expand.y {
                output.size_mut().y.set_max(frame.height());
            }

            let width = frame.width();
            let x = if self.config.columns.dir == Dir::LTR {
                offset
            } else {
                regions.size.x - offset - width
            };
            offset += width
                + self.config.columns.gutters.get(i).copied().unwrap_or_default();

            // During distribution, the baseline of the region is set to the
            // baseline of the first frame - e.g., the first paragraph line.
            // However, this does not propagate outwards unless we also update
            // the baseline of the column that region is in.
            if i == 0 && frame.has_baseline() {
                output.set_baseline(frame.baseline());
            }

            output.push_frame(Point::with_x(x), frame);
            inner.next();
        }

        Ok(output)
    }

    /// Lay out a column, including column insertions.
    fn column(&mut self, locator: Locator, regions: Regions) -> FlowResult<Frame> {
        // Reset column insertion when starting a new column.
        self.column_insertions = Insertions::default();
        self.deferred.clear();
        self.cuts.clear();
        self.uncuttable.clear();

        // Process footnote spill.
        for spill in std::mem::take(&mut self.work.footnote_spill) {
            self.footnote_spill(spill, regions.base())?;
        }

        // This loop can restart column layout when requested to do so by a
        // `Stop`. This happens when there is a column-scoped float.
        let checkpoint = self.work.clone();
        let initial = (
            self.column_insertions.clone(),
            self.footnote_spill.clone(),
            self.footnote_queue.clone(),
        );
        let inner = loop {
            // Shrink the available space by the space used by column
            // insertions.
            let mut pod = regions;
            pod.size.y -= self.column_insertions.height();
            // (level notes in a balanced region: the column's reserve at its foot)
            pod.size.y -= self.reserves.get(self.column).copied().unwrap_or_default();

            self.balance = None;
            match self.column_contents(pod) {
                Ok(frame) => {
                    // An entry must never precede its marker. If a marker
                    // whose entry was reserved here didn't make it into this
                    // column after all, lay out the column again from
                    // scratch, letting the marker's frame move on instead.
                    // Each round defers at least one more footnote, so this
                    // terminates. A deferred marker's frame only moves on if
                    // the region may progress, so this can't loop across
                    // regions either.
                    let orphans = self.orphaned_footnotes(&frame);
                    if orphans.is_empty() {
                        break frame;
                    }
                    self.deferred.extend(orphans);
                    *self.work = checkpoint.clone();
                    self.column_insertions = initial.0.clone();
                    self.footnote_spill = initial.1.clone();
                    self.footnote_queue = initial.2.clone();
                    continue;
                }
                Err(Stop::Finish(_)) => unreachable!(),
                Err(Stop::Relayout(PlacementScope::Column)) => {
                    *self.work = checkpoint.clone();
                    continue;
                }
                err => return err,
            }
        };
        drop(checkpoint);

        self.used += self.flow_used + self.column_insertions.height();
        self.work.footnotes.extend(self.footnote_queue.drain(..));
        self.work.footnote_spill = std::mem::take(&mut self.footnote_spill);

        let insertions = std::mem::take(&mut self.column_insertions);
        self.extents.push((insertions.top_size + self.flow_used, insertions.bottom_size));
        let mut output = insertions.finalize(self.work, self.config, inner);

        // Lay out per-column line numbers.
        if let Some(line_config) = &self.config.line_numbers {
            layout_line_numbers(
                self.engine,
                self.config,
                line_config,
                locator,
                self.column,
                &mut output,
            )?;
        }

        Ok(output)
    }

    /// Lay out the inner contents of a column.
    ///
    /// Pending floats and footnotes are also laid out at this step. For those,
    /// however, we forbid footnote migration (moving the frame containing the
    /// footnote reference if the corresponding entry doesn't fit), allowing
    /// the footnote invariant to be broken, as it would require handling a
    /// [`Stop::Finish`] at this point, but that is exclusively handled by the
    /// distributor.
    fn column_contents(&mut self, regions: Regions) -> FlowResult<Frame> {
        // Process pending footnotes.
        let ctx = NoteCtx {
            flow_need: Abs::zero(),
            migratable: false,
            atomic: false,
            cuttable: false,
            fresh: false,
        };
        for note in std::mem::take(&mut self.work.footnotes) {
            self.footnote(note, &mut regions.clone(), ctx)?;
        }

        // Process pending floats.
        for placed in std::mem::take(&mut self.work.floats) {
            self.float(placed, &regions, false, false)?;
        }

        distribute(self, regions)
    }

    /// Lays out an item with floating placement.
    ///
    /// This is called from within [`distribute()`]. When the float fits, this
    /// returns an `Err(Stop::Relayout(..))`, which bubbles all the way through
    /// distribution and is handled in [`Self::page`] or [`Self::column`]
    /// (depending on `placed.scope`).
    ///
    /// When the float does not fit, it is queued into `work.floats`. The
    /// value of `clearance` indicates that between the float and flow content
    /// is needed --- it is set if there are already distributed items.
    ///
    /// The value of `migratable` determines whether footnotes within the float
    /// should be allowed to prompt its migration if they don't fit in order to
    /// respect the footnote invariant (entries in the same page as the
    /// references), triggering [`Stop::Finish`]. This is usually `true` within
    /// the distributor, as it can handle that particular flow event, and
    /// `false` elsewhere.
    pub fn float(
        &mut self,
        placed: &'b PlacedChild<'a>,
        regions: &Regions,
        clearance: bool,
        migratable: bool,
    ) -> FlowResult<()> {
        // If the float is already processed, skip it.
        let loc = placed.location();
        if self.skipped(loc) {
            return Ok(());
        }

        // If there is already a queued float, queue this one as well. We
        // don't want to disrupt the order.
        if !self.work.floats.is_empty() {
            self.work.floats.push(placed);
            return Ok(());
        }

        // Determine the base size of the chosen scope.
        let base = match placed.scope {
            PlacementScope::Column => regions.base(),
            PlacementScope::Parent => self.page_base,
        };

        // Lay out the placed element.
        let frame = placed.layout(self.engine, base)?;

        // Determine the remaining space in the scope. This is exact for column
        // placement, but only an approximation for page placement.
        let remaining = match placed.scope {
            PlacementScope::Column => regions.size.y,
            PlacementScope::Parent => {
                let remaining: Abs = regions
                    .iter()
                    .map(|size| size.y)
                    .take(self.config.columns.count - self.column)
                    .sum();
                remaining / self.config.columns.count as f64
            }
        };

        // We only require clearance if there is other content.
        let clearance = if clearance { placed.clearance } else { Abs::zero() };
        let need = frame.height() + clearance;

        // If the float doesn't fit, queue it for the next region.
        if !remaining.fits(need) && regions.may_progress() {
            self.work.floats.push(placed);
            return Ok(());
        }

        // Handle footnotes in the float.
        self.footnotes(regions, &frame, need, false, migratable, false)?;

        // Determine the float's vertical alignment. We can unwrap the inner
        // `Option` because `Custom(None)` is checked for during collection.
        let align_y = placed.align_y.map(Option::unwrap).unwrap_or_else(|| {
            // When the float's vertical midpoint would be above the middle of
            // the page if it were layouted in-flow, we use top alignment.
            // Otherwise, we use bottom alignment.
            let used = base.y - remaining;
            let half = need / 2.0;
            let ratio = (used + half) / base.y;
            if ratio <= 0.5 { FixedAlignment::Start } else { FixedAlignment::End }
        });

        // Select the insertion area where we'll put this float.
        let area = match placed.scope {
            PlacementScope::Column => &mut self.column_insertions,
            PlacementScope::Parent => &mut self.page_insertions,
        };

        // Put the float there.
        area.push_float(placed, frame, align_y);
        area.skips.push(loc);

        // Trigger relayout.
        Err(Stop::Relayout(placed.scope))
    }

    /// Lays out footnotes in the `frame` if this is the root flow and there are
    /// any. The value of `breakable` indicates whether the element that
    /// produced the frame is breakable. If not, the frame is treated as atomic.
    ///
    /// The value of `migratable` indicates whether footnote migration should be
    /// possible (at least for the first footnote found in the frame, as it is
    /// forbidden for the second footnote onwards). It is usually `true` within
    /// the distributor and `false` elsewhere, as the distributor can handle
    /// [`Stop::Finish`] which is returned when migration is requested.
    ///
    /// The value of `cuttable` indicates that the frame is the current frame
    /// of a breakable block in the distributor, which handles
    /// [`Stop::Cut`] to break the block above a marker whose entry doesn't
    /// fit below it.
    pub fn footnotes(
        &mut self,
        regions: &Regions,
        frame: &Frame,
        flow_need: Abs,
        breakable: bool,
        migratable: bool,
        cuttable: bool,
    ) -> FlowResult<()> {
        // Footnotes are only supported at the root level.
        if self.config.mode != FlowMode::Root {
            return Ok(());
        }

        // Search for footnotes.
        let mut notes = vec![];
        for tag in &self.work.tags {
            let Tag::Start(elem, _) = tag else { continue };
            let Some(note) = elem.to_packed::<FootnoteElem>() else { continue };
            notes.push((Abs::zero(), note.clone()));
        }
        find_in_frame_impl::<FootnoteElem>(&mut notes, frame, Abs::zero());
        if notes.is_empty() {
            return Ok(());
        }

        let mut relayout = false;
        let mut regions = *regions;

        // The origin frame should be migratable if the region may progress
        // and if the origin frame isn't breakable. This holds for every
        // footnote in the frame, not just the first: if a later entry doesn't
        // fit, the frame migrates and the entries already reserved for it are
        // orphaned, which `Composer::column` detects and undoes. Whether the
        // region may progress must be decided before reserving any entries,
        // as they shrink `regions`.
        let atomic = migratable && !breakable && regions.may_progress();

        for (y, elem) in notes {
            // The amount of space used by the in-flow content that contains the
            // footnote marker. For a breakable frame, it's the y position of
            // the marker. For an unbreakable frame, it's the full height.
            let flow_need = if breakable { y } else { flow_need };

            // Process the footnote.
            let ctx = NoteCtx {
                flow_need,
                migratable: atomic,
                atomic,
                cuttable: cuttable && breakable,
                fresh: true,
            };
            match self.footnote(elem, &mut regions, ctx) {
                // The footnote was already processed or queued.
                Ok(()) => {}
                // First handle more footnotes before relayouting.
                Err(Stop::Relayout(_)) => relayout = true,
                // Either of
                // - A `Stop::Finish` indicating that the frame's origin element
                //   should migrate to uphold the footnote invariant.
                // - A `Stop::Cut` indicating that the breakable origin block
                //   should break above the marker.
                // - A fatal error.
                err => return err,
            }
        }

        // If this is set, we laid out at least one footnote, so we need a
        // relayout.
        if relayout {
            return Err(Stop::Relayout(PlacementScope::Column));
        }

        Ok(())
    }

    /// Handles a single footnote.
    fn footnote(
        &mut self,
        elem: Packed<FootnoteElem>,
        regions: &mut Regions,
        ctx: NoteCtx,
    ) -> FlowResult<()> {
        let NoteCtx { flow_need, migratable, .. } = ctx;

        // Ignore reference footnotes and already processed ones.
        let loc = elem.location().unwrap();
        if elem.is_ref() || self.skipped(loc) {
            return Ok(());
        }

        // An earlier layout of this column reserved this entry, but then
        // moved its marker to a later region. Move the marker's frame (or
        // line, in a breakable block) on right away, so that it is followed
        // by its entry. If that's impossible, queue the entry.
        if self.deferred.contains(&loc) {
            if ctx.atomic && regions.may_progress() {
                return Err(Stop::Finish(false));
            } else if self.may_cut(&ctx, loc, regions) {
                return Err(Stop::Cut(flow_need, loc));
            }
            self.footnote_queue.push(elem);
            return Ok(());
        }

        let lane = elem.lane.get(self.config.shared);
        let (x, width) = footnote_lane(self.config, lane, regions.size.x, elem.span())?;

        let may_cut = self.may_cut(&ctx, loc, regions);

        // Preserve entry order within each lane (and within the full-width
        // listing), without blocking unrelated lanes behind a long entry.
        let area = &mut self.column_insertions;
        if self.footnote_spill.iter().any(|spill| spill.lane == lane)
            || self
                .footnote_queue
                .iter()
                .any(|note| note.lane.get(self.config.shared) == lane)
        {
            self.footnote_queue.push(elem);
            return Ok(());
        }

        // If there weren't any footnotes so far, account for the footnote
        // separator.
        let mut separator = None;
        let mut separator_need = Abs::zero();
        if area.footnote_separator.is_none() {
            let frame =
                layout_footnote_separator(self.engine, self.config, regions.base())?;
            separator_need += self.config.footnote.clearance + frame.height();
            separator = Some(frame);
        }

        // Prepare regions for the footnote.
        let mut pod = *regions;
        pod.size.x = width;
        pod.expand.y = false;
        // Other lanes may already reserve more height than this lane uses.
        // Reuse that space before increasing the shared insertion height.
        pod.size.y += area.lane_slack(lane);
        pod.size.y -= flow_need + separator_need + self.config.footnote.gap;

        // Layout the footnote entry.
        let mut frames =
            layout_footnote(self.engine, self.config, &elem, pod)?.into_frames();
        mark_continuations(self.engine, self.config, &elem, &mut frames)?;

        // Find nested footnotes in the entry.
        let nested = find_in_frames::<FootnoteElem>(&frames);

        // Check if there are any non-empty frames.
        let exist_non_empty_frame = frames.iter().any(|f| !f.is_empty());

        // Extract the first frame.
        let mut iter = frames.into_iter();
        let first = iter.next().unwrap();

        // If the first frame is empty, then none of its content fit. If
        // possible, we then migrate the origin frame to the next region to
        // uphold the footnote invariant (that marker and entry are on the same
        // page). If not, we just queue the footnote for the next page, but
        // only if that would actually make a difference (that is, if the
        // footnote isn't alone in the page after not fitting in any previous
        // pages, as it probably won't ever fit then).
        //
        // Note that a non-zero flow need also indicates that queueing would
        // make a difference, because the flow need is subtracted from the
        // available height in the entry's pod even if what caused that need
        // wasn't considered for the input `regions`. For example, floats just
        // pass the `regions` they received along to their footnotes, which
        // don't take into account the space occupied by the floats themselves,
        // but they do indicate their footnotes have a non-zero flow need, so
        // queueing them can matter as, in the following pages, the flow need
        // will be set to zero and the footnote will be alone in the page.
        // Then, `may_progress()` will also be false (this time, correctly) and
        // the footnote is laid out, as queueing wouldn't improve the lack of
        // space anymore and would result in an infinite loop.
        //
        // However, it is worth noting that migration does take into account
        // the original region, before inserting what prompted the flow need.
        // Logically, if moving the original frame can't improve the lack of
        // space, then migration should be inhibited. The space occupied by the
        // original frame is not relevant for that check. Therefore,
        // `regions.may_progress()` must still be checked separately for
        // migration, regardless of the presence of flow need.
        //
        // A breakable origin block can't migrate as a whole, but it can break
        // above the marker's line, so that line moves to the next region
        // together with the entry, just like an atomic frame would.
        if first.is_empty() && exist_non_empty_frame {
            if migratable && regions.may_progress() {
                return Err(Stop::Finish(false));
            } else if may_cut {
                return Err(Stop::Cut(flow_need, loc));
            } else if regions.may_progress() || !flow_need.is_zero() {
                self.footnote_queue.push(elem);
                return Ok(());
            }
        }

        // Save the separator.
        if let Some(frame) = separator {
            area.push_footnote_separator(self.config, frame);
            regions.size.y -= separator_need;
        }

        // Save the footnote's frame.
        let before = area.height();
        area.push_footnote(self.config, lane, x, first);
        area.skips.push(loc);
        if ctx.fresh {
            area.fresh.push(loc);
        }
        if let Some(lane) = lane
            && !area.has_lane_separator(lane)
        {
            let base = Size::new(width, regions.base().y);
            let frame = layout_footnote_separator(self.engine, self.config, base)?;
            area.push_lane_separator(lane, x, frame);
        }
        regions.size.y -= area.height() - before;

        // Save the spill.
        if !iter.as_slice().is_empty() {
            self.footnote_spill
                .push(FootnoteSpill { lane, x, width, frames: iter });
        }

        // Lay out nested footnotes. Their markers are in this entry, so they
        // are not tied to the in-flow frame.
        let nested_ctx = NoteCtx {
            atomic: false,
            cuttable: false,
            fresh: false,
            ..ctx
        };
        for (_, note) in nested {
            match self.footnote(note, regions, nested_ctx) {
                // This footnote was already processed or queued.
                Ok(_) => {}
                // Footnotes always request a relayout when processed for the
                // first time, so we ignore a relayout request since we're
                // about to do so afterwards. Without this check, the first
                // inner footnote interrupts processing of the following ones.
                Err(Stop::Relayout(_)) => {}
                // Either of
                // - A `Stop::Finish` indicating that the frame's origin element
                //   should migrate to uphold the footnote invariant.
                // - A fatal error.
                err => return err,
            }
        }

        // Since we laid out a footnote, we need a relayout.
        Err(Stop::Relayout(PlacementScope::Column))
    }

    /// Handles spillover from a footnote.
    fn footnote_spill(
        &mut self,
        mut spill: FootnoteSpill,
        base: Size,
    ) -> SourceResult<()> {
        let area = &mut self.column_insertions;

        // Create and save the separator.
        if area.footnote_separator.is_none() {
            let separator = layout_footnote_separator(self.engine, self.config, base)?;
            area.push_footnote_separator(self.config, separator);
        }

        // Save the footnote's frame.
        let frame = spill.frames.next().unwrap();
        area.push_footnote(self.config, spill.lane, spill.x, frame);
        if let Some(lane) = spill.lane
            && !area.has_lane_separator(lane)
        {
            let base = Size::new(spill.width, base.y);
            let frame = layout_footnote_separator(self.engine, self.config, base)?;
            area.push_lane_separator(lane, spill.x, frame);
        }

        // Save the spill.
        if !spill.frames.as_slice().is_empty() {
            self.footnote_spill.push(spill);
        }

        Ok(())
    }

    /// Whether the breakable block containing the marker of footnote `loc` may
    /// be asked to break above the marker (see [`Stop::Cut`]). The
    /// distributor only accepts a cut that makes progress.
    fn may_cut(&self, ctx: &NoteCtx, loc: Location, regions: &Regions) -> bool {
        ctx.cuttable && regions.may_break() && !self.uncuttable.contains(&loc)
    }

    /// The height limit for the first frame of the breakable block `id` in
    /// this column, if it must break above a marker.
    pub fn cut(&self, id: usize) -> Option<Abs> {
        self.cuts.iter().filter(|&&(i, _)| i == id).map(|&(_, cap)| cap).min()
    }

    /// Footnotes whose entries were laid out for this column's flow, but whose
    /// markers are not in the column's `inner` frame or its insertions.
    fn orphaned_footnotes(&self, inner: &Frame) -> Vec<Location> {
        let fresh = &self.column_insertions.fresh;
        if fresh.is_empty() {
            return vec![];
        }

        let mut found = vec![];
        find_in_frame_impl::<FootnoteElem>(&mut found, inner, Abs::zero());
        for insertions in [&self.column_insertions, &self.page_insertions] {
            for (_, frame) in
                insertions.top_floats.iter().chain(&insertions.bottom_floats)
            {
                find_in_frame_impl::<FootnoteElem>(&mut found, frame, Abs::zero());
            }
        }
        for (_, _, frame) in &self.column_insertions.footnotes {
            find_in_frame_impl::<FootnoteElem>(&mut found, frame, Abs::zero());
        }

        let present: Vec<Location> =
            found.iter().filter_map(|(_, note)| note.location()).collect();
        fresh.iter().filter(|loc| !present.contains(loc)).copied().collect()
    }

    /// Checks whether an insertion was already processed and doesn't need to be
    /// handled again.
    fn skipped(&self, loc: Location) -> bool {
        self.work.skips.contains(&loc)
            || self.page_insertions.skips.contains(&loc)
            || self.column_insertions.skips.contains(&loc)
    }

    /// The amount of width needed by insertions.
    pub fn insertion_width(&self) -> Abs {
        self.column_insertions.width.max(self.page_insertions.width)
    }
}

/// Resolves the horizontal offset and width of a footnote lane within the
/// footnote area of the given width. Entries without a lane span the full
/// width.
fn footnote_lane(
    config: &Config,
    lane: Option<usize>,
    width: Abs,
    span: Span,
) -> SourceResult<(Abs, Abs)> {
    let Some(lane) = lane else { return Ok((Abs::zero(), width)) };
    let lanes = &config.footnote.lanes;
    let gutter = config.footnote.lane_gutter;
    if lane >= lanes.len() {
        bail!(
            span, "footnote lane {lane} does not exist";
            hint: "{} lanes are configured with `footnote.entry(lanes: ..)`", lanes.len();
        );
    }
    if !width.is_finite() {
        bail!(span, "footnote lanes require a finite page width");
    }
    if gutter < Abs::zero() {
        bail!(span, "footnote lane gutter must not be negative");
    }

    // Distribute the space left by fixed lanes and gutters among fractional
    // lanes, just like grid columns.
    let mut fixed = gutter * (lanes.len() - 1) as f64;
    let mut fr = Fr::zero();
    for sizing in lanes {
        match *sizing {
            LaneWidth::Rel(rel) => fixed += rel.relative_to(width),
            LaneWidth::Fr(v) => fr += v,
        }
    }
    let remaining = width - fixed;
    if !remaining.fits(Abs::zero()) || !fr.get().is_finite() {
        bail!(span, "footnote lanes and gutters are wider than the footnote area");
    }

    let resolve = |sizing: &LaneWidth| match *sizing {
        LaneWidth::Rel(rel) => rel.relative_to(width),
        LaneWidth::Fr(v) => v.share(fr, remaining),
    };

    let x = lanes[..lane].iter().map(|s| resolve(s) + gutter).sum();
    let size = resolve(&lanes[lane]);
    if size <= Abs::zero() {
        bail!(span, "footnote lane {lane} has no width");
    }
    Ok((x, size))
}

/// Lay out the footnote separator, typically a line.
fn layout_footnote_separator(
    engine: &mut Engine,
    config: &Config,
    base: Size,
) -> SourceResult<Frame> {
    crate::layout_frame(
        engine,
        &config.footnote.separator,
        Locator::root(),
        config.shared,
        Region::new(base, Axes::new(config.footnote.expand, false)),
    )
}

/// Lay out a footnote.
fn layout_footnote(
    engine: &mut Engine,
    config: &Config,
    elem: &Packed<FootnoteElem>,
    pod: Regions,
) -> SourceResult<Fragment> {
    let loc = elem.location().unwrap();
    crate::layout_fragment(
        engine,
        &FootnoteEntry::new(elem.clone())
            .pack()
            .spanned(elem.span())
            // We attach a well-known derived location to the entry so that the
            // note can link to this entry without first querying for it.
            .located(loc.variant(1)),
        Locator::synthesize(loc),
        config.shared,
        pod,
    )
    .map(|mut fragment| {
        for frame in &mut fragment {
            frame.set_parent(FrameParent::new(loc, Inherit::No));
        }
        fragment
    })
}

/// Adds the configured continuation cue (see `footnote.entry.continuation`)
/// to each continued part of a split entry: every frame with content after
/// the first one with content.
fn mark_continuations(
    engine: &mut Engine,
    config: &Config,
    elem: &Packed<FootnoteElem>,
    frames: &mut [Frame],
) -> SourceResult<()> {
    let Some(continuation) = &config.footnote.continuation else {
        return Ok(());
    };
    let Some(start) = frames.iter().position(has_content) else {
        return Ok(());
    };

    let loc = elem.location().unwrap();
    let mut cue = None;
    let mut k = 0;
    for frame in &mut frames[start + 1..] {
        if !has_content(frame) {
            continue;
        }
        let cue = match &mut cue {
            Some(cue) => cue,
            None => {
                // The entry's own text styles (but not those of a show rule).
                let entry = Packed::new(FootnoteEntry::new(elem.clone()));
                let styles = entry.show_set(config.shared);
                let content = continuation
                    .resolve(engine, config.shared, elem)?
                    .styled_with_map(styles)
                    .artifact(ArtifactKind::Other)
                    .spanned(elem.span());
                cue.insert(content)
            }
        };

        // Each cue gets its own well-known locations, distinct from the
        // entry's (variant 1) and from the other parts' cues.
        k += 1;
        let mut marker = crate::layout_frame(
            engine,
            cue,
            Locator::synthesize(loc.variant(1 + k)),
            config.shared,
            Region::new(Axes::splat(Abs::inf()), Axes::splat(false)),
        )?;
        let marker_baseline = marker.baseline();
        marker.translate(Point::with_y(-marker_baseline));

        // Hang the cue before the first line of the continued part, aligned
        // to its baseline.
        let pos = match first_line(frame) {
            Some((baseline, _, end)) if config.columns.dir == Dir::RTL => {
                Point::new(end, baseline)
            }
            Some((baseline, start, _)) => Point::new(start - marker.width(), baseline),
            None => Point::new(-marker.width(), marker_baseline),
        };
        frame.push_frame(pos, marker);
    }

    Ok(())
}

/// The baseline and horizontal text extent of a frame's first line of text.
fn first_line(frame: &Frame) -> Option<(Abs, Abs, Abs)> {
    fn collect(frame: &Frame, offset: Point, out: &mut Vec<(Point, Abs, Abs)>) {
        for (pos, item) in frame.items() {
            let pos = offset + *pos;
            match item {
                FrameItem::Group(group) => {
                    // Only follow plain translations; transformed content
                    // doesn't define a line.
                    if group.transform.is_identity() {
                        collect(&group.frame, pos, out);
                    }
                }
                FrameItem::Text(text) => {
                    out.push((pos, text.width(), text.size));
                }
                _ => {}
            }
        }
    }

    let mut runs = vec![];
    collect(frame, Point::zero(), &mut runs);
    let top = runs.iter().map(|(pos, ..)| pos.y).min()?;
    let size = runs.iter().map(|&(.., size)| size).max()?;
    // Raised text (like a superscript) on the first line has a baseline a
    // little above the line's, but still well below half the text size; the
    // next line is at least about one text size further down.
    let line = runs.iter().filter(|(pos, ..)| pos.y <= top + size * 0.6);
    let mut baseline = top;
    let mut start = Abs::inf();
    let mut end = -Abs::inf();
    for &(pos, width, _) in line {
        baseline.set_max(pos.y);
        start.set_min(pos.x);
        end.set_max(pos.x + width);
    }
    Some((baseline, start, end))
}

/// Whether the frame has visible content (anything besides tags and links).
pub fn has_content(frame: &Frame) -> bool {
    frame.items().any(|(_, item)| match item {
        FrameItem::Group(group) => has_content(&group.frame),
        FrameItem::Tag(_) | FrameItem::Link(..) => false,
        _ => true,
    })
}

/// An additive list of insertions.
#[derive(Default, Clone)]
struct Insertions<'a, 'b> {
    top_floats: Vec<(&'b PlacedChild<'a>, Frame)>,
    bottom_floats: Vec<(&'b PlacedChild<'a>, Frame)>,
    footnotes: Vec<(Option<usize>, Abs, Frame)>,
    lane_heights: Vec<Abs>,
    footnote_separator: Option<Frame>,
    /// Separators of occupied lanes: lane, x offset and frame.
    lane_separators: Vec<(usize, Abs, Frame)>,
    /// Footnotes laid out for markers in this column's flow.
    fresh: Vec<Location>,
    top_size: Abs,
    bottom_size: Abs,
    width: Abs,
    skips: Vec<Location>,
}

impl<'a, 'b> Insertions<'a, 'b> {
    /// Add a float to the top or bottom area.
    fn push_float(
        &mut self,
        placed: &'b PlacedChild<'a>,
        frame: Frame,
        align_y: FixedAlignment,
    ) {
        self.width.set_max(frame.width());

        let amount = frame.height() + placed.clearance;
        let pair = (placed, frame);

        if align_y == FixedAlignment::Start {
            self.top_size += amount;
            self.top_floats.push(pair);
        } else {
            self.bottom_size += amount;
            self.bottom_floats.push(pair);
        }
    }

    /// Add a footnote to the bottom area.
    fn push_footnote(
        &mut self,
        config: &Config,
        lane: Option<usize>,
        x: Abs,
        frame: Frame,
    ) {
        self.width.set_max(x + frame.width());
        let amount = config.footnote.gap + frame.height();
        if let Some(lane) = lane {
            let before = self.lanes_height();
            self.lane_heights.resize(config.footnote.lanes.len(), Abs::zero());
            self.lane_heights[lane] += amount;
            self.bottom_size += self.lanes_height() - before;
        } else {
            self.bottom_size += amount;
        }
        self.footnotes.push((lane, x, frame));
    }

    /// The height reserved for all lanes together.
    fn lanes_height(&self) -> Abs {
        self.lane_heights.iter().copied().max().unwrap_or_default()
    }

    /// Space that this lane can use without growing the insertion area.
    fn lane_slack(&self, lane: Option<usize>) -> Abs {
        lane.map_or(Abs::zero(), |lane| {
            self.lanes_height() - self.lane_heights.get(lane).copied().unwrap_or_default()
        })
    }

    /// Add a footnote separator to the bottom area.
    fn push_footnote_separator(&mut self, config: &Config, frame: Frame) {
        self.width.set_max(frame.width());
        self.bottom_size += config.footnote.clearance + frame.height();
        self.footnote_separator = Some(frame);
    }

    /// Whether the lane already has its own separator.
    fn has_lane_separator(&self, lane: usize) -> bool {
        self.lane_separators.iter().any(|&(l, _, _)| l == lane)
    }

    /// The height of the separator row: all separators share it.
    fn separator_height(&self) -> Abs {
        self.footnote_separator
            .iter()
            .chain(self.lane_separators.iter().map(|(_, _, frame)| frame))
            .map(Frame::height)
            .max()
            .unwrap_or_default()
    }

    /// Add the separator of a lane that just became occupied. It shares the
    /// separator row with the (full-width) footnote separator, which must
    /// have been pushed already.
    fn push_lane_separator(&mut self, lane: usize, x: Abs, frame: Frame) {
        let before = self.separator_height();
        self.width.set_max(x + frame.width());
        self.lane_separators.push((lane, x, frame));
        self.bottom_size += self.separator_height() - before;
    }

    /// The combined height of the top and bottom area (including clearances).
    /// Subtracting this from the total region size yields the available space
    /// for distribution.
    fn height(&self) -> Abs {
        self.top_size + self.bottom_size
    }

    /// Produce a frame for the full region based on the `inner` frame produced
    /// by distribution or column layout.
    fn finalize(self, work: &mut Work, config: &Config, inner: Frame) -> Frame {
        work.extend_skips(&self.skips);

        if self.top_floats.is_empty()
            && self.bottom_floats.is_empty()
            && self.footnote_separator.is_none()
            && self.footnotes.is_empty()
        {
            return inner;
        }

        let size = inner.size() + Size::with_y(self.height());
        let separator_height = self.separator_height();

        let mut output = Frame::soft(size);
        let mut offset_top = Abs::zero();
        let mut offset_bottom = size.y - self.bottom_size;

        for (placed, frame) in self.top_floats {
            let x = placed.align_x.position(size.x - frame.width());
            let y = offset_top;
            let delta = placed.delta.zip_map(size, Rel::relative_to).to_point();
            offset_top += frame.height() + placed.clearance;
            output.push_frame(Point::new(x, y) + delta, frame);
        }

        // Technically, if the flow has floats and footnotes, it's probably not
        // going to be within a block but correspond to the main page flow,
        // where the baseline doesn't matter. However, we'd keep the baseline
        // correct anyways in case future enhancements enable deeper
        // manipulation of this flow.
        if inner.has_baseline() {
            output.set_baseline(self.top_size + inner.baseline());
        }

        output.push_frame(Point::with_y(self.top_size), inner);

        // We put floats first and then footnotes. This differs from what LaTeX
        // does and is a little inconsistent w.r.t column vs page floats (page
        // floats are below footnotes because footnotes are per column), but
        // it's what most people (including myself) seem to intuitively expect.
        // We experimented with the LaTeX ordering in 0.12.0-rc1, but folks were
        // surprised and considered this strange. In LaTeX, it can be changed
        // with `\usepackage[bottom]{footmisc}`. We could also consider adding
        // configuration in the future.
        for (placed, frame) in self.bottom_floats {
            offset_bottom += placed.clearance;
            let x = placed.align_x.position(size.x - frame.width());
            let y = offset_bottom;
            let delta = placed.delta.zip_map(size, Rel::relative_to).to_point();
            offset_bottom += frame.height();
            output.push_frame(Point::new(x, y) + delta, frame);
        }

        // With lanes, each occupied lane has its own separator at its left
        // edge, so that no separator stands above an empty lane. Full-width
        // entries are listed above all lanes, so if there are any, a single
        // separator spans the area as without lanes.
        if let Some(frame) = self.footnote_separator {
            offset_bottom += config.footnote.clearance;
            let y = offset_bottom;
            offset_bottom += separator_height;
            if self.lane_separators.is_empty()
                || self.footnotes.iter().any(|(lane, _, _)| lane.is_none())
            {
                output.push_frame(Point::with_y(y), frame);
            } else {
                for (_, x, frame) in self.lane_separators {
                    output.push_frame(Point::new(x, y), frame);
                }
            }
        }

        // Full-width entries precede all lane entries, regardless of marker
        // order. Within each listing, preserve the original entry order.
        for (_, _, frame) in self.footnotes.iter().filter(|(lane, _, _)| lane.is_none()) {
            offset_bottom += config.footnote.gap;
            let y = offset_bottom;
            offset_bottom += frame.height();
            output.push_frame(Point::with_y(y), frame.clone());
        }

        let mut offsets = vec![offset_bottom; self.lane_heights.len()];
        for (lane, x, frame) in self.footnotes {
            let Some(lane) = lane else { continue };
            offsets[lane] += config.footnote.gap;
            let y = offsets[lane];
            offsets[lane] += frame.height();
            output.push_frame(Point::new(x, y), frame);
        }

        output
    }
}

/// Lay out the given collected lines' line numbers to an output frame.
///
/// The numbers are placed either on the left margin (left border of the frame)
/// or on the right margin (right border). Before they are placed, a line number
/// counter reset is inserted if we're in the first column of the page being
/// currently laid out and the user requested for line numbers to be reset at
/// the start of every page.
fn layout_line_numbers(
    engine: &mut Engine,
    config: &Config,
    line_config: &LineNumberConfig,
    locator: Locator,
    column: usize,
    output: &mut Frame,
) -> SourceResult<()> {
    let mut locator = locator.split();

    // Reset page-scoped line numbers if currently at the first column.
    if column == 0 && line_config.scope == LineNumberingScope::Page {
        let reset = layout_line_number_reset(engine, config, &mut locator)?;
        output.push_frame(Point::zero(), reset);
    }

    // Find all line markers.
    let mut lines = find_in_frame::<ParLineMarker>(output);
    if lines.is_empty() {
        return Ok(());
    }

    // Assume the line numbers aren't sorted by height. They must be sorted so
    // we can deduplicate line numbers below based on vertical proximity.
    lines.sort_by_key(|&(y, _)| y);

    // Used for horizontal alignment.
    let mut max_number_width = Abs::zero();

    // This is used to skip lines that are too close together.
    let mut prev_bottom = None;

    // Buffer line number frames so we can align them horizontally later before
    // placing, based on the width of the largest line number.
    let mut line_numbers = vec![];

    // Layout the lines.
    for &(y, ref marker) in &lines {
        if prev_bottom.is_some_and(|bottom| y < bottom) {
            // Lines are too close together. Display as the same line number.
            continue;
        }

        // Layout the number and record its width in search of the maximum.
        let frame = layout_line_number(engine, config, &mut locator, &marker.numbering)?;

        // Note that this line.y is larger than the previous due to sorting.
        // Therefore, the check at the top of the loop ensures no line numbers
        // will reasonably intersect with each other. We enforce a minimum
        // spacing of 1pt between consecutive line numbers in case a zero-height
        // frame is used.
        prev_bottom = Some(y + frame.height().max(Abs::pt(1.0)));
        max_number_width.set_max(frame.width());
        line_numbers.push((y, marker, frame));
    }

    for (y, marker, frame) in line_numbers {
        // The last column will always place line numbers at the end
        // margin. This should become configurable in the future.
        let margin = {
            let opposite =
                config.columns.count >= 2 && column + 1 == config.columns.count;
            if opposite { OuterHAlignment::End } else { marker.number_margin }
                .resolve(config.shared)
        };

        // Determine how much space to leave between the column and the number.
        let clearance = match marker.number_clearance {
            Smart::Auto => line_config.default_clearance,
            Smart::Custom(rel) => rel.resolve(config.shared),
        };

        // Compute the base X position.
        let x = match margin {
            // Move the number to the left of the left edge (at 0pt) by the maximum
            // width and the clearance.
            FixedAlignment::Start => -max_number_width - clearance,
            // Move the number to the right edge and add clearance.
            FixedAlignment::End => output.width() + clearance,
            // Can't happen due to `OuterHAlignment`.
            FixedAlignment::Center => unreachable!(),
        };

        // Determine how much to shift the number due to its alignment.
        let shift = {
            let align = marker
                .number_align
                .map(|align| align.resolve(config.shared))
                .unwrap_or_else(|| margin.inv());
            align.position(max_number_width - frame.width())
        };

        // Compute the final position of the number and add it to the output.
        let pos = Point::new(x + shift, y);
        output.push_frame(pos, frame);
    }

    Ok(())
}

/// Creates a frame that resets the line number counter.
fn layout_line_number_reset(
    engine: &mut Engine,
    config: &Config,
    locator: &mut SplitLocator,
) -> SourceResult<Frame> {
    let counter = Counter::of(ParLineMarker::ELEM);
    let update = CounterUpdate::Set(CounterState::init(false));
    let content = counter.update(Span::detached(), update);
    crate::layout_frame(
        engine,
        &content,
        locator.next(&()),
        config.shared,
        Region::new(Axes::splat(Abs::zero()), Axes::splat(false)),
    )
}

/// Layout the line number associated with the given line marker.
///
/// Produces a counter update and counter display with counter key
/// `ParLineMarker`. We use `ParLineMarker` as it is an element which is not
/// exposed to the user and we don't want to expose the line number counter at
/// the moment, given that its semantics are inconsistent with that of normal
/// counters (the counter is updated based on height and not on frame order /
/// layer). When we find a solution to this, we should switch to a counter on
/// `ParLine` instead, thus exposing the counter as `counter(par.line)` to the
/// user.
fn layout_line_number(
    engine: &mut Engine,
    config: &Config,
    locator: &mut SplitLocator,
    numbering: &Numbering,
) -> SourceResult<Frame> {
    let counter = Counter::of(ParLineMarker::ELEM);
    let update = CounterUpdate::Step(NonZeroUsize::ONE);
    let numbering = Smart::Custom(numbering.clone());

    // Combine counter update and display into the content we'll layout.
    let content = Content::sequence(vec![
        counter.clone().update(Span::detached(), update),
        CounterDisplayElem::new(counter, numbering, false).pack(),
    ]);
    let content = content.artifact(ArtifactKind::LineNumber);

    // Layout the number.
    let mut frame = crate::layout_frame(
        engine,
        &content,
        locator.next(&()),
        config.shared,
        Region::new(Axes::splat(Abs::inf()), Axes::splat(false)),
    )?;

    // Ensure the baseline of the line number aligns with the line's baseline.
    frame.translate(Point::with_y(-frame.baseline()));

    Ok(frame)
}

/// Collect all matching elements and their vertical positions in the frame.
///
/// On each subframe we encounter, we add that subframe's position to `prev_y`,
/// until we reach a tag, at which point we add the tag's position and finish.
/// That gives us the absolute height of the tag from the start of the root
/// frame.
fn find_in_frame<T: NativeElement>(frame: &Frame) -> Vec<(Abs, Packed<T>)> {
    let mut output = vec![];
    find_in_frame_impl(&mut output, frame, Abs::zero());
    output
}

/// Collect all matching elements and their vertical positions in the frames.
fn find_in_frames<T: NativeElement>(frames: &[Frame]) -> Vec<(Abs, Packed<T>)> {
    let mut output = vec![];
    for frame in frames {
        find_in_frame_impl(&mut output, frame, Abs::zero());
    }
    output
}

fn find_in_frame_impl<T: NativeElement>(
    output: &mut Vec<(Abs, Packed<T>)>,
    frame: &Frame,
    y_offset: Abs,
) {
    for (pos, item) in frame.items() {
        let y = y_offset + pos.y;
        match item {
            FrameItem::Group(group) => find_in_frame_impl(output, &group.frame, y),
            FrameItem::Tag(Tag::Start(elem, _)) => {
                if let Some(elem) = elem.to_packed::<T>() {
                    output.push((y, elem.clone()));
                }
            }
            _ => {}
        }
    }
}
