use comemo::{Track, Tracked, TrackedMut};
use typst_library::diag::SourceResult;
use typst_library::engine::{Engine, Route, Sink, Traced};
use typst_library::foundations::{
    Content, NativeElement, Resolve, Smart, StyleChain, Styles,
};
use typst_library::introspection::{
    Counter, CounterDisplayElem, CounterKey, Introspector, Locator, LocatorLink,
};
use typst_library::layout::{
    Abs, AlignElem, Alignment, Axes, Binding, ColumnsElem, Dir, Frame, HAlignment,
    Length, OuterVAlignment, PageElem, Paper, Region, Regions, Rel, Sides, Size,
    VAlignment,
};
use typst_library::model::{ArtifactKind, Numbering};
use typst_library::routines::Pair;
use typst_library::text::{LocalName, TextElem};
use typst_library::visualize::Paint;
use typst_library::{Library, World};
use typst_utils::{LazyHash, Numeric, Protected};

use std::sync::Arc;

use typst_library::introspection::SplitLocator;

use crate::flow::{ColumnOptions, FlowMode, WorkSnap, layout_flow, layout_flow_inc};
use crate::incremental::{self, FlowInc, Marks, Recs, RunLog, with_tees};

/// A mostly finished layout for one page. Needs only knowledge of its exact
/// page number to be finalized into a `Page`. (Because the margins can depend
/// on the page number.)
#[derive(Clone)]
pub struct LayoutedPage {
    pub inner: Frame,
    pub margin: Sides<Abs>,
    pub margin_two_sided: bool,
    pub bleed: Sides<Abs>,
    pub bleed_two_sided: bool,
    pub binding: Binding,
    pub header: Option<Frame>,
    pub footer: Option<Frame>,
    pub background: Option<Frame>,
    pub foreground: Option<Frame>,
    pub fill: Smart<Option<Paint>>,
    pub numbering: Option<Numbering>,
    pub supplement: Content,
}

/// Layout a single page suitable  for parity adjustment.
pub fn layout_blank_page(
    engine: &mut Engine,
    locator: Locator,
    initial: StyleChain,
) -> SourceResult<LayoutedPage> {
    let layouted = layout_page_run(engine, &[], locator, initial)?;
    Ok(layouted.into_iter().next().unwrap())
}

/// Layout a page run with uniform properties.
#[typst_macros::time(name = "page run")]
pub fn layout_page_run(
    engine: &mut Engine,
    children: &[Pair],
    locator: Locator,
    initial: StyleChain,
) -> SourceResult<Vec<LayoutedPage>> {
    layout_page_run_impl(
        engine.world,
        engine.library,
        engine.introspector.into_raw(),
        engine.traced,
        TrackedMut::reborrow_mut(&mut engine.sink),
        engine.route.track(),
        children,
        locator.track(),
        initial,
    )
}

/// The internal implementation of `layout_page_run`.
#[comemo::memoize]
#[expect(clippy::too_many_arguments)]
fn layout_page_run_impl(
    world: Tracked<dyn World + '_>,
    library: &LazyHash<Library>,
    introspector: Tracked<dyn Introspector + '_>,
    traced: Tracked<Traced>,
    sink: TrackedMut<Sink>,
    route: Tracked<Route>,
    children: &[Pair],
    locator: Tracked<Locator>,
    initial: StyleChain,
) -> SourceResult<Vec<LayoutedPage>> {
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

    // Determine the page-wide styles.
    let styles = Styles::root(children, initial);
    let styles = StyleChain::new(&styles);

    // When one of the lengths is infinite the page fits its content along
    // that axis.
    let width = styles.resolve(PageElem::width).unwrap_or(Abs::inf());
    let height = styles.resolve(PageElem::height).unwrap_or(Abs::inf());
    let mut size = Size::new(width, height);
    if styles.get(PageElem::flipped) {
        std::mem::swap(&mut size.x, &mut size.y);
    }

    let mut min = width.min(height);
    if !min.is_finite() {
        min = Paper::A4.width();
    }

    // Determine the margins.
    let default = Rel::<Length>::from((2.5 / 21.0) * min);
    let margin = styles.get(PageElem::margin).unwrap_or_default();
    let margin_two_sided = margin.two_sided.unwrap_or(false);
    let margin = margin
        .sides
        .map(|side| side.and_then(Smart::custom).unwrap_or(default))
        .resolve(styles)
        .relative_to(size);

    let bleed = styles.get(PageElem::bleed);
    let bleed_two_sided = bleed.two_sided.unwrap_or(false);
    let bleed = bleed
        .sides
        .map(|side| side.unwrap_or(Rel::zero()))
        .resolve(styles)
        .relative_to(size);

    let fill = styles.get_cloned(PageElem::fill);
    let foreground = styles.get_ref(PageElem::foreground);
    let background = styles.get_ref(PageElem::background);
    let header_ascent = styles.resolve(PageElem::header_ascent).relative_to(margin.top);
    let footer_descent =
        styles.resolve(PageElem::footer_descent).relative_to(margin.bottom);
    let numbering = styles.get_ref(PageElem::numbering);
    let supplement = match styles.get_cloned(PageElem::supplement) {
        Smart::Auto => TextElem::packed(PageElem::local_name_in(styles)),
        Smart::Custom(content) => content.unwrap_or_default(),
    };
    let number_align = styles.get(PageElem::number_align);
    let binding = styles.get(PageElem::binding).unwrap_or_else(|| {
        match styles.resolve(TextElem::dir) {
            Dir::LTR => Binding::Left,
            _ => Binding::Right,
        }
    });

    // Construct the numbering (for header or footer).
    let numbering_marginal = numbering.as_ref().map(|numbering| {
        let both = match numbering {
            Numbering::Pattern(pattern) => pattern.pieces() >= 2,
            Numbering::Func(_) => true,
        };

        let mut counter = CounterDisplayElem::new(
            Counter::new(CounterKey::Page),
            Smart::Custom(numbering.clone()),
            both,
        )
        .pack();

        // We interpret the Y alignment as selecting header or footer
        // and then ignore it for aligning the actual number.
        if let Some(x) = number_align.x() {
            counter = counter.aligned(x.into());
        }

        counter
    });

    let header = styles.get_ref(PageElem::header);
    let footer = styles.get_ref(PageElem::footer);
    let (header, footer) = if matches!(number_align.y(), Some(OuterVAlignment::Top)) {
        (header.as_ref().unwrap_or(&numbering_marginal), footer.as_ref().unwrap_or(&None))
    } else {
        (header.as_ref().unwrap_or(&None), footer.as_ref().unwrap_or(&numbering_marginal))
    };

    // Layout the children.
    let area = size - margin.sum_by_axis();

    // (fork) incremental layout of a long run: see `crate::incremental`.
    if incremental::mode() > 0 && children.len() >= incremental::min_children() {
        let header = header.clone().map(|h| h.artifact(ArtifactKind::Header));
        let footer = footer.clone().map(|f| f.artifact(ArtifactKind::Footer));
        let background = background.clone().map(|b| b.artifact(ArtifactKind::Background));
        let key = typst_utils::hash128(&(link.key(), styles, area));
        drop(locator);
        if std::env::var("TYPST_INC_LOG").as_deref() == Ok("2") {
            eprintln!(
                "typst-inc: key {key:032x}: link {:032x} styles {:032x} area {:?}",
                link.key(),
                typst_utils::hash128(&styles),
                area
            );
            for (i, style) in styles.entries().enumerate() {
                let span = style.span();
                let line = span.id().and_then(|id| {
                    let source = engine.world.source(id).ok()?;
                    let range = source.find(span)?.range();
                    Some((
                        format!("{:?}", id.vpath()),
                        source.lines().byte_to_line(range.start).map_or(0, |l| l + 1),
                    ))
                });
                eprintln!(
                    "typst-inc: style {i} {} {:?} {:032x}",
                    style.element().map_or("-", |e| e.name()),
                    line,
                    typst_utils::hash128(style)
                );
            }
        }

        let page_of = |engine: &mut Engine,
                       locator: &mut SplitLocator,
                       inner: Frame|
         -> SourceResult<LayoutedPage> {
            let mut layout_marginal = |content: &Option<Content>, area, align| {
                let Some(content) = content else { return Ok(None) };
                let aligned = content.clone().set(AlignElem::alignment, align);
                crate::layout_frame(
                    engine,
                    &aligned,
                    locator.next(&content.span()),
                    styles,
                    Region::new(area, Axes::splat(true)),
                )
                .map(Some)
            };
            let header_size = Size::new(inner.width(), margin.top - header_ascent);
            let footer_size = Size::new(inner.width(), margin.bottom - footer_descent);
            let full_size = inner.size() + margin.sum_by_axis() + bleed.sum_by_axis();
            let mid = HAlignment::Center + VAlignment::Horizon;
            Ok(LayoutedPage {
                fill: fill.clone(),
                numbering: numbering.clone(),
                supplement: supplement.clone(),
                header: layout_marginal(&header, header_size, Alignment::BOTTOM)?,
                footer: layout_marginal(&footer, footer_size, Alignment::TOP)?,
                background: layout_marginal(&background, full_size, mid)?,
                foreground: layout_marginal(foreground, full_size, mid)?,
                inner,
                margin,
                margin_two_sided,
                bleed,
                bleed_two_sided,
                binding,
            })
        };

        let run = |engine: &mut Engine,
                   candidates: Vec<Arc<RunLog<WorkSnap>>>|
         -> SourceResult<(Vec<LayoutedPage>, RunLog<WorkSnap>, usize, bool)> {
            let mut locator = Locator::link(&link).split();
            let marg_candidates = candidates.clone();
            let mut inc = FlowInc { candidates, resumed: None, spliced: 0, out: None };
            let fragment = layout_flow_inc(
                engine,
                children,
                &mut locator,
                styles,
                Regions::repeat(area, area.map(Abs::is_finite)),
                ColumnOptions {
                    count: styles.get(PageElem::columns),
                    balanced: styles.get(ColumnsElem::balanced),
                    gutter: styles.get_cloned(ColumnsElem::gutter).resolve(styles),
                    separator: styles.get_cloned(ColumnsElem::separator),
                },
                &mut inc,
            )?;
            let t_marg = std::time::Instant::now();
            let resumed = inc.resumed.as_ref().map_or(0, |r| r.1) + inc.spliced;
            // The marginals, page by page: a page's are taken from the newest
            // log whose page there had the same size and whose marginals' reads
            // give the same results (they read nothing else of the page).
            let mut reuse = 0;
            let mut marg = Vec::with_capacity(fragment.len());
            let mut pages = Vec::with_capacity(fragment.len());
            for (j, inner) in fragment.into_iter().enumerate() {
                let found = marg_candidates.iter().take(2).find_map(|log| {
                    let old = log.pages.get(j)?;
                    let calls = log.marg.get(j)?;
                    (old.inner.size() == inner.size() && calls.check(engine, Marks::default(), calls.end()))
                        .then(|| (old, calls.clone()))
                });
                if let Some((old, calls)) = found {
                    calls.replay(engine, calls.end());
                    // (the locator counts the marginals it would have laid out)
                    for m in [&header, &footer, &background, foreground] {
                        if let Some(c) = m {
                            locator.next(&c.span());
                        }
                    }
                    pages.push(LayoutedPage { inner, ..old.clone() });
                    marg.push(calls);
                    reuse += 1;
                } else {
                    let mrecs = Recs::default();
                    let page = with_tees(engine, &mrecs, |engine| page_of(engine, &mut locator, inner))?;
                    pages.push(page);
                    marg.push(Arc::new(mrecs.take()));
                }
            }
            if incremental::log_on() {
                eprintln!(
                    "typst-inc: run {} pages ({} marginals taken): marginals {} ms",
                    pages.len(),
                    reuse,
                    t_marg.elapsed().as_millis()
                );
            }
            let log = RunLog {
                flow: inc.out.take().unwrap(),
                pages: pages.clone(),
                marg,
            };
            // (taken whole from one log: every region from its start, every page's marginals)
            let whole = inc.spliced == 0 && resumed == pages.len() && reuse == pages.len();
            Ok((pages, log, resumed, whole))
        };

        let candidates = incremental::candidates::<WorkSnap>(key);
        let (pages, log, resumed, whole) = run(&mut engine, candidates)?;
        if incremental::mode() == 2 && resumed > 0 {
            let (full, flog, _, _) = run(&mut engine, vec![])?;
            let h = |p: &LayoutedPage| {
                typst_utils::hash128(&(
                    &p.inner,
                    &p.header,
                    &p.footer,
                    &p.background,
                    &p.foreground,
                ))
            };
            let first_diff = (0..pages.len().max(full.len()))
                .find(|&i| pages.get(i).map(h) != full.get(i).map(h));
            match first_diff {
                None => eprintln!(
                    "typst-inc: check: {} pages identical ({} regions taken)",
                    full.len(),
                    resumed
                ),
                Some(i) => eprintln!(
                    "typst-inc: check: MISMATCH at page {} of {} (full) / {} (resumed), {} regions taken",
                    i + 1,
                    full.len(),
                    pages.len(),
                    resumed
                ),
            }
            incremental::put(key, flog);
            return Ok(full);
        }
        // (a layout taken whole from a log is that log again: not kept twice, so the logs kept
        // stay distinct — with logs kept past a compile, a trial's next candidate still finds
        // the last one's first iteration, laid out on the same empty introspection)
        if whole {
            incremental::touch::<WorkSnap>(key);
        } else {
            incremental::put(key, log);
        }
        return Ok(pages);
    }

    let fragment = layout_flow(
        &mut engine,
        children,
        &mut locator,
        styles,
        Regions::repeat(area, area.map(Abs::is_finite)),
        ColumnOptions {
            count: styles.get(PageElem::columns),
            balanced: styles.get(ColumnsElem::balanced),
            gutter: styles.get_cloned(ColumnsElem::gutter).resolve(styles),
            separator: styles.get_cloned(ColumnsElem::separator),
        },
        FlowMode::Root,
    )?;

    // Layouts a single marginal.
    let mut layout_marginal = |content: &Option<Content>, area, align| {
        let Some(content) = content else { return Ok(None) };
        let aligned = content.clone().set(AlignElem::alignment, align);
        crate::layout_frame(
            &mut engine,
            &aligned,
            locator.next(&content.span()),
            styles,
            Region::new(area, Axes::splat(true)),
        )
        .map(Some)
    };

    // Layout marginals.
    let mut layouted = Vec::with_capacity(fragment.len());

    let header = header.clone().map(|h| h.artifact(ArtifactKind::Header));
    let footer = footer.clone().map(|f| f.artifact(ArtifactKind::Footer));
    let background = background.clone().map(|b| b.artifact(ArtifactKind::Background));

    let t_marg = std::time::Instant::now();
    let n_frag = fragment.len();
    for inner in fragment {
        let header_size = Size::new(inner.width(), margin.top - header_ascent);
        let footer_size = Size::new(inner.width(), margin.bottom - footer_descent);
        let full_size = inner.size() + margin.sum_by_axis() + bleed.sum_by_axis();
        let mid = HAlignment::Center + VAlignment::Horizon;
        layouted.push(LayoutedPage {
            inner,
            fill: fill.clone(),
            numbering: numbering.clone(),
            supplement: supplement.clone(),
            header: layout_marginal(&header, header_size, Alignment::BOTTOM)?,
            footer: layout_marginal(&footer, footer_size, Alignment::TOP)?,
            background: layout_marginal(&background, full_size, mid)?,
            foreground: layout_marginal(foreground, full_size, mid)?,
            margin,
            margin_two_sided,
            bleed,
            bleed_two_sided,
            binding,
        });
    }

    if n_frag > 1 && std::env::var_os("TYPST_INC_LOG").is_some() {
        eprintln!("typst-inc: run {} pages: marginals {} ms", n_frag, t_marg.elapsed().as_millis());
    }
    Ok(layouted)
}
