//! Layout of content into a [`Frame`] or [`Fragment`].

mod block;
mod collect;
mod compose;
mod distribute;

pub(crate) use self::block::unbreakable_pod;

use std::num::NonZeroUsize;
use std::rc::Rc;

use bumpalo::Bump;
use comemo::{Track, Tracked, TrackedMut};
use ecow::EcoVec;
use rustc_hash::FxHashSet;
use typst_library::diag::{At, SourceDiagnostic, SourceResult, bail};
use typst_library::engine::{Engine, Route, Sink, Traced};
use typst_library::foundations::{Content, Packed, Resolve, StyleChain};
use typst_library::introspection::{
    Introspector, Location, Locator, LocatorLink, SplitLocator, Tag,
};
use typst_library::layout::{
    Abs, Angle, ColumnGutters, ColumnsElem, Dir, Em, Fr, Fragment, Frame, HAlignment,
    PageElem, PlacementScope, Region, Regions, Rel, Size, Spacing, VAlignment,
};
use typst_library::model::ArtifactKind;
use typst_library::model::{
    FootnoteContinuation, FootnoteElem, FootnoteEntry, LineNumberingScope, ParLine,
};
use typst_library::routines::{Arenas, FragmentKind, Pair, RealizationKind};
use typst_library::text::TextElem;
use typst_library::visualize::LineElem;
use typst_library::{Library, World};
use typst_utils::{LazyHash, NonZeroExt, Numeric, Protected};

use self::block::{layout_multi_block, layout_single_block};
use self::collect::{
    BalanceSpec, Child, LineChild, MultiChild, MultiSpill, PlacedChild, SingleChild,
    collect,
};
use self::compose::compose;

/// Lays out content into a single region, producing a single frame.
pub fn layout_frame(
    engine: &mut Engine,
    content: &Content,
    locator: Locator,
    styles: StyleChain,
    region: Region,
) -> SourceResult<Frame> {
    layout_fragment(engine, content, locator, styles, region.into())
        .map(Fragment::into_frame)
}

/// Lays out content into multiple regions.
///
/// When laying out into just one region, prefer [`layout_frame`].
pub fn layout_fragment(
    engine: &mut Engine,
    content: &Content,
    locator: Locator,
    styles: StyleChain,
    regions: Regions,
) -> SourceResult<Fragment> {
    layout_fragment_impl(
        engine.world,
        engine.library,
        engine.introspector.into_raw(),
        engine.traced,
        TrackedMut::reborrow_mut(&mut engine.sink),
        engine.route.track(),
        content,
        locator.track(),
        styles,
        regions,
        ColumnOptions {
            count: NonZeroUsize::ONE,
            balanced: false,
            gutter: ColumnGutters::uniform(Rel::zero()),
            separator: None,
        },
    )
}

/// Layout the columns.
///
/// This is different from just laying out into column-sized regions as the
/// columns can interact due to parent-scoped placed elements.
#[typst_macros::time(span = elem.span())]
pub fn layout_columns(
    elem: &Packed<ColumnsElem>,
    engine: &mut Engine,
    locator: Locator,
    styles: StyleChain,
    regions: Regions,
) -> SourceResult<Fragment> {
    layout_fragment_impl(
        engine.world,
        engine.library,
        engine.introspector.into_raw(),
        engine.traced,
        TrackedMut::reborrow_mut(&mut engine.sink),
        engine.route.track(),
        &elem.body,
        locator.track(),
        styles,
        regions,
        ColumnOptions {
            count: elem.count.get(styles),
            balanced: elem.balanced.get(styles),
            gutter: elem.gutter.resolve(styles),
            separator: elem.separator.get_cloned(styles),
        },
    )
}

/// The cached, internal implementation of [`layout_fragment`].
#[comemo::memoize]
#[expect(clippy::too_many_arguments)]
fn layout_fragment_impl(
    world: Tracked<dyn World + '_>,
    library: &LazyHash<Library>,
    introspector: Tracked<dyn Introspector + '_>,
    traced: Tracked<Traced>,
    sink: TrackedMut<Sink>,
    route: Tracked<Route>,
    content: &Content,
    locator: Tracked<Locator>,
    styles: StyleChain,
    regions: Regions,
    column: ColumnOptions,
) -> SourceResult<Fragment> {
    if !regions.size.x.is_finite() && regions.expand.x {
        bail!(content.span(), "cannot expand into infinite width");
    }
    if !regions.size.y.is_finite() && regions.expand.y {
        bail!(content.span(), "cannot expand into infinite height");
    }

    let introspector = Protected::from_raw(introspector);
    let link = LocatorLink::new(locator);
    let mut locator = Locator::link(&link).split();
    let mut engine = Engine {
        library,
        world,
        introspector,
        traced,
        sink,
        route: Route::extend(route),
    };

    engine.route.check_layout_depth().at(content.span())?;

    let mut kind = FragmentKind::Block;
    let arenas = Arenas::default();
    let children = (engine.library.routines.realize)(
        RealizationKind::Fragment { kind: &mut kind },
        &mut engine,
        &mut locator,
        &arenas,
        content,
        styles,
    )?;

    layout_flow(
        &mut engine,
        &children,
        &mut locator,
        styles,
        regions,
        column,
        kind.into(),
    )
}

/// The mode a flow can be laid out in.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FlowMode {
    /// A root flow with block-level elements. Like `FlowMode::Block`, but can
    /// additionally host footnotes and line numbers.
    Root,
    /// A flow whose children are block-level elements.
    Block,
    /// A flow whose children are inline-level elements.
    Inline,
}

impl From<FragmentKind> for FlowMode {
    fn from(value: FragmentKind) -> Self {
        match value {
            FragmentKind::Inline => Self::Inline,
            FragmentKind::Block => Self::Block,
        }
    }
}

/// Lays out realized content into regions, potentially with columns.
pub fn layout_flow<'a>(
    engine: &mut Engine,
    children: &[Pair<'a>],
    locator: &mut SplitLocator<'a>,
    shared: StyleChain<'a>,
    mut regions: Regions,
    column: ColumnOptions,
    mode: FlowMode,
) -> SourceResult<Fragment> {
    // Prepare configuration that is shared across the whole flow.
    let config = configuration(shared, regions, column, mode);

    // Collect the elements into pre-processed children. These are much easier
    // to handle than the raw elements.
    let bump = Bump::new();
    let children = collect(
        engine,
        &bump,
        children,
        locator.next(&()),
        Size::new(config.columns.width, regions.full),
        regions.expand.x,
        mode,
    )?;

    let mut work = Work::new(&children);
    let mut finished = vec![];

    // This loop runs once per region produced by the flow layout.
    loop {
        let region_locator = locator.next(&());
        let before = work.clone();
        let mut frame =
            compose(engine, &mut work, &config, region_locator.relayout(), regions, false)?;

        // A region that took nothing from the work, followed by a region just
        // like it, would be laid out the same way again, forever: e.g. a line
        // whose footnote entries leave too little room for the lines grouped
        // with it by widow/orphan prevention moves on from the top of a column
        // (as the entries, reserved for it, shrink the region, the next one
        // always looks roomier), and meets the same situation in the next
        // region. Then the region is laid out again with the content at the
        // top of each column pinned there: it stays even if it doesn't fit
        // (an orphan or widow, an entry continued or moved to the next page).
        // A flow that makes progress never takes this path.
        if work.stalled(&before) && regions.repeats() {
            work = before;
            frame =
                compose(engine, &mut work, &config, region_locator.relayout(), regions, true)?;
        }
        finished.push(frame);

        // Terminate the loop when everything is processed, though draining the
        // backlog if necessary.
        if work.done() && (!regions.expand.y || regions.backlog.is_empty()) {
            break;
        }

        regions.next();
    }

    Ok(Fragment::frames(finished))
}

/// Determine the flow's configuration.
fn configuration<'x>(
    shared: StyleChain<'x>,
    regions: Regions,
    column: ColumnOptions,
    mode: FlowMode,
) -> Config<'x> {
    Config {
        mode,
        shared,
        columns: {
            let mut count = column.count.get();
            if !regions.size.x.is_finite() {
                count = 1;
            }

            let base = regions.base().x;
            let gutters: Vec<Abs> = (0..count.saturating_sub(1))
                .map(|i| column.gutter.get(i, base))
                .collect();
            let total = gutters.iter().fold(Abs::zero(), |a, &g| a + g);
            let width = (regions.size.x - total) / count as f64;
            let dir = shared.resolve(TextElem::dir);
            ColumnConfig {
                count,
                width,
                gutters,
                dir,
                balanced: column.balanced,
                separator: column.separator.map(|separator| {
                    separator
                        .set(LineElem::length, Rel::one())
                        .set(LineElem::angle, Angle::deg(90.0))
                        .aligned(HAlignment::Center + VAlignment::Horizon)
                }),
            }
        },
        footnote: FootnoteConfig {
            separator: shared
                .get_cloned(FootnoteEntry::separator)
                .artifact(ArtifactKind::Other),
            clearance: shared.resolve(FootnoteEntry::clearance),
            gap: shared.resolve(FootnoteEntry::gap),
            lanes: shared
                .get_ref(FootnoteEntry::lanes)
                .iter()
                .map(|width| match *width {
                    Spacing::Rel(rel) => LaneWidth::Rel(rel.resolve(shared)),
                    Spacing::Fr(fr) => LaneWidth::Fr(fr),
                })
                .collect(),
            lane_gutter: shared.resolve(FootnoteEntry::lane_gutter),
            continuation: shared.get_cloned(FootnoteEntry::continuation),
            expand: regions.expand.x,
        },
        line_numbers: (mode == FlowMode::Root).then(|| LineNumberConfig {
            scope: shared.get(ParLine::numbering_scope),
            counted: shared.get(ParLine::counted),
            default_clearance: {
                let width = if shared.get(PageElem::flipped) {
                    shared.resolve(PageElem::height)
                } else {
                    shared.resolve(PageElem::width)
                };

                // Clamp below is safe (min <= max): if the font size is
                // negative, we set min = max = 0; otherwise,
                // `0.75 * size <= 2.5 * size` for zero and positive sizes.
                (0.026 * width.unwrap_or_default()).clamp(
                    Em::new(0.75).resolve(shared).max(Abs::zero()),
                    Em::new(2.5).resolve(shared).max(Abs::zero()),
                )
            },
        }),
    }
}

/// The work that is left to do by flow layout.
///
/// The lifetimes 'a and 'b are used across flow layout:
/// - 'a is that of the content coming out of realization
/// - 'b is that of the collected/prepared children
#[derive(Clone)]
struct Work<'a, 'b> {
    /// Children that we haven't processed yet. This slice shrinks over time.
    children: &'b [Child<'a>],
    /// Leftovers from a breakable block.
    spill: Option<MultiSpill<'a, 'b>>,
    /// Queued floats that didn't fit in previous regions.
    floats: EcoVec<&'b PlacedChild<'a>>,
    /// Queued footnotes that didn't fit in previous regions.
    footnotes: EcoVec<Packed<FootnoteElem>>,
    /// Spilled frames of a footnote that didn't fully fit. Similar to `spill`.
    footnote_spill: Vec<FootnoteSpill>,
    /// Queued tags that will be attached to the next frame.
    tags: EcoVec<&'a Tag>,
    /// Identifies floats and footnotes that can be skipped if visited because
    /// they were already handled and incorporated as column or page level
    /// insertions.
    skips: Rc<FxHashSet<Location>>,
}

impl<'a, 'b> Work<'a, 'b> {
    /// Create the initial work state from a list of children.
    fn new(children: &'b [Child<'a>]) -> Self {
        Self {
            children,
            spill: None,
            floats: EcoVec::new(),
            footnotes: EcoVec::new(),
            footnote_spill: vec![],
            tags: EcoVec::new(),
            skips: Rc::new(FxHashSet::default()),
        }
    }

    /// Get the first unprocessed child, from the start of the slice.
    fn head(&self) -> Option<&'b Child<'a>> {
        self.children.first()
    }

    /// Mark the `head()` child as processed, advancing the slice by one.
    fn advance(&mut self) {
        self.children = &self.children[1..];
    }

    /// Whether all work is done. This means we can terminate flow layout.
    /// Whether a region laid out from the `before` state took nothing from
    /// the work, so that a region just like it would be laid out the same way.
    /// (Leftovers of a breakable block may have shrunk: never stalled.)
    fn stalled(&self, before: &Self) -> bool {
        self.children.len() == before.children.len()
            && self.spill.is_none()
            && before.spill.is_none()
            && self.floats.len() == before.floats.len()
            && self.footnotes.len() == before.footnotes.len()
            && self.footnote_spill.len() == before.footnote_spill.len()
            && self
                .footnote_spill
                .iter()
                .zip(&before.footnote_spill)
                .all(|(a, b)| a.frames.len() == b.frames.len())
            && self.skips.len() == before.skips.len()
    }

    fn done(&self) -> bool {
        self.children.is_empty()
            && self.spill.is_none()
            && self.floats.is_empty()
            && self.footnote_spill.is_empty()
            && self.footnotes.is_empty()
    }

    /// Add skipped floats and footnotes from the insertion areas to the skip
    /// set.
    fn extend_skips(&mut self, skips: &[Location]) {
        if !skips.is_empty() {
            Rc::make_mut(&mut self.skips).extend(skips.iter().copied());
        }
    }
}

/// Options defining the column layout.
#[derive(Hash)]
pub struct ColumnOptions {
    /// The number of columns.
    pub count: NonZeroUsize,
    /// Whether column heights are to be equalized.
    pub balanced: bool,
    /// The spacing between columns.
    pub gutter: ColumnGutters,
    /// The separator between columns.
    pub separator: Option<Content>,
}

/// Shared configuration for the whole flow.
struct Config<'x> {
    /// Whether this is the root flow, which can host footnotes and line
    /// numbers.
    mode: FlowMode,
    /// The styles shared by the whole flow. This is used for footnotes and line
    /// numbers.
    shared: StyleChain<'x>,
    /// Settings for columns.
    columns: ColumnConfig,
    /// Settings for footnotes.
    footnote: FootnoteConfig,
    /// Settings for line numbers.
    line_numbers: Option<LineNumberConfig>,
}

/// Configuration of footnotes.
struct FootnoteConfig {
    /// The separator between flow content and footnotes. Typically a line.
    separator: Content,
    /// The amount of space left above the separator.
    clearance: Abs,
    /// The gap between footnote entries.
    gap: Abs,
    /// Lane widths. Relative parts resolve against the column width.
    lanes: Vec<LaneWidth>,
    /// The horizontal gap between lanes.
    lane_gutter: Abs,
    /// The cue at the start of a split entry's continued parts.
    continuation: Option<FootnoteContinuation>,
    /// Whether horizontal expansion is enabled for footnotes.
    expand: bool,
}

/// Width of a footnote lane.
#[derive(Debug, Copy, Clone)]
enum LaneWidth {
    Rel(Rel<Abs>),
    Fr(Fr),
}

/// Remaining frames of a split entry, retaining its lane across regions.
#[derive(Clone)]
struct FootnoteSpill {
    lane: Option<usize>,
    x: Abs,
    width: Abs,
    frames: std::vec::IntoIter<Frame>,
}

/// Configuration of columns.
struct ColumnConfig {
    /// The number of columns.
    count: usize,
    /// The width of each column.
    width: Abs,
    /// The amount of space after each column but the last.
    gutters: Vec<Abs>,
    /// The horizontal direction in which columns progress. Defined by
    /// `text.dir`.
    dir: Dir,
    /// Whether to equalize the height of columns by breaking columns early.
    balanced: bool,
    /// The separator between columns.
    separator: Option<Content>,
}

/// Configuration of line numbers.
struct LineNumberConfig {
    /// Where line numbers are reset.
    scope: LineNumberingScope,
    /// (fork) Whether the numbers count (`par.line(counted:)`).
    counted: bool,
    /// The default clearance for `auto`.
    ///
    /// This value should be relative to the page's width, such that the
    /// clearance between line numbers and text is small when the page is,
    /// itself, small. However, that could cause the clearance to be too small
    /// or too large when considering the current text size; in particular, a
    /// larger text size would require more clearance to be able to tell line
    /// numbers apart from text, whereas a smaller text size requires less
    /// clearance so they aren't way too far apart. Therefore, the default
    /// value is a percentage of the page width clamped between `0.75em` and
    /// `2.5em`.
    default_clearance: Abs,
}

/// The result type for flow layout.
///
/// The `Err(_)` variant incorporate control flow events for finishing and
/// relayouting regions.
type FlowResult<T> = Result<T, Stop>;

/// A control flow event during flow layout.
enum Stop {
    /// Indicates that the current subregion should be finished. Can be caused
    /// by a lack of space (`false`) or an explicit column break (`true`).
    Finish(bool),
    /// Indicates that the given scope should be relayouted.
    Relayout(PlacementScope),
    /// Indicates that the entry of the footnote `loc`, whose marker sits at
    /// height `y` in a breakable frame, does not fit below the marker. The
    /// breakable block should break above the marker's line so that marker
    /// and entry can move to the next region together. Only produced for
    /// frames of breakable blocks and handled by the distributor.
    Cut(Abs, Location),
    /// A fatal error.
    Error(EcoVec<SourceDiagnostic>),
}

impl From<EcoVec<SourceDiagnostic>> for Stop {
    fn from(error: EcoVec<SourceDiagnostic>) -> Self {
        Stop::Error(error)
    }
}
