use std::num::NonZeroUsize;

use smallvec::{SmallVec, smallvec};

use crate::diag::{HintedStrResult, bail};
use crate::foundations::{
    Array, Cast, Content, IntoValue, Resolve, StyleChain, Value, cast, elem,
};
use crate::layout::{Abs, Length, Ratio, Rel};

/// Separates a region into multiple equally sized columns.
///
/// The `columns` function separates the interior of a container into multiple
/// columns. By default, columns take the height of the container or remaining
/// page and fill one after another. Use @columns.balanced to equalize their
/// height, or @colbalance to balance the columns up to a point.
///
/// When arranging content across multiple columns, use @colbreak to explicitly
/// continue in the next column.
///
/// = Example <example>
/// ```example
/// #columns(2, gutter: 8pt)[
///   This text is in the
///   first column.
///
///   #colbreak()
///
///   This text is in the
///   second column.
/// ]
/// ```
///
/// = #short-or-long[Page Level][Page-level columns] <page-level>
/// If you need to insert columns across your whole document, use the `{page}`
/// function's @page.columns[`columns` parameter] instead. This will create the
/// columns directly at the page-level rather than wrapping all of your content
/// in a layout container. As a result, things like @pagebreak[pagebreaks],
/// @footnote[footnotes], and @par.line[line numbers] will continue to work as
/// expected. For more information, also read the
/// @guides:page-setup:columns[relevant part of the page setup guide].
///
/// = #short-or-long[Breaking Out][Breaking out of columns] <breaking-out>
/// To temporarily break out of columns (e.g. for a paper's title), use
/// parent-scoped floating placement:
///
/// #example(
///   single: true,
///   ```
///   #set page(columns: 2, height: 150pt)
///
///   #place(
///     top + center,
///     scope: "parent",
///     float: true,
///     text(1.4em, weight: "bold")[
///       My document
///     ],
///   )
///
///   #lorem(40)
///   ```
/// )
#[elem(since = "forever")]
pub struct ColumnsElem {
    /// The number of columns.
    #[positional]
    #[default(NonZeroUsize::new(2).unwrap())]
    pub count: NonZeroUsize,

    /// The size of the gutter space between each column.
    ///
    /// Either one size for every gutter, or an array with one size per
    /// gutter, from the first column onwards (if the array is shorter than
    /// the number of gutters, its last size repeats). The columns share the
    /// width that the gutters leave equally.
    ///
    /// ```example
    /// #columns(4, gutter: (4pt, 16pt, 4pt))[
    ///   #for i in range(1, 5) [Column #i #colbreak()]
    /// ]
    /// ```
    #[default(ColumnGutter(smallvec![Ratio::new(0.04).into()]))]
    pub gutter: ColumnGutter,

    /// Whether to equalize the height of columns by breaking columns early.
    ///
    /// ```example
    /// #set page(columns: 2, height: 5cm)
    /// #set par(justify: true)
    /// >>> #set columns(gutter: 15pt)
    /// #set columns(balanced: true)
    /// #lorem(30)
    /// ```
    #[default(false)]
    pub balanced: bool,

    /// The separator to place between each pair of non-empty columns.
    ///
    /// By default, the content is centered and @line[lines] use a length of
    /// `{100%}` and an angle of `{90deg}` unless specified otherwise.
    ///
    /// ```example
    /// #set page(columns: 2, height: 5cm)
    /// #set par(justify: true)
    /// #set columns(
    ///   gutter: 2em,
    ///   separator: line(),
    /// )
    /// #lorem(35)
    /// ```
    ///
    /// #example(
    ///   title: "Separator with extended, dotted line",
    ///   ```
    ///   #set page(columns: 2, height: 5cm)
    ///   #set par(justify: true)
    ///   #set columns(
    ///     gutter: 2em,
    ///     separator: line(
    ///       stroke: (dash: "dotted"),
    ///       length: 100% + 2 * 4pt,
    ///     ),
    ///   )
    ///   #lorem(35)
    ///   ```
    /// )
    ///
    /// #example(
    ///   title: "Custom separator ornament",
    ///   ```
    ///   #set page(columns: 3, width: 15cm, height: 3cm)
    ///   #set par(justify: true)
    ///   #set columns(
    ///     gutter: 30pt,
    ///     separator: curve(curve.cubic((-8pt, 0%), (8pt, 50%), (0pt, 50%))
    ///   ))
    ///   #lorem(36)
    ///   ```
    /// )
    #[default]
    pub separator: Option<Content>,

    /// The content that should be layouted into the columns.
    #[required]
    pub body: Content,
}

/// The gutters between columns: one size for all of them, or one per gutter.
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct ColumnGutter(pub SmallVec<[Rel<Length>; 1]>);

impl Resolve for ColumnGutter {
    type Output = ColumnGutters;

    fn resolve(self, styles: StyleChain) -> Self::Output {
        ColumnGutters(self.0.into_iter().map(|g| g.resolve(styles)).collect())
    }
}

cast! {
    ColumnGutter,
    self => if self.0.len() == 1 {
        self.0[0].into_value()
    } else {
        self.0.into_iter().map(IntoValue::into_value).collect::<Array>().into_value()
    },
    v: Rel<Length> => Self(smallvec![v]),
    values: Array => {
        let gutters: SmallVec<_> = values.into_iter().map(Value::cast).collect::<HintedStrResult<_>>()?;
        if gutters.is_empty() {
            bail!("column gutter array must not be empty");
        }
        Self(gutters)
    },
}

/// Resolved column gutters (see [`ColumnGutter`]).
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct ColumnGutters(pub SmallVec<[Rel<Abs>; 1]>);

impl ColumnGutters {
    /// One size for every gutter.
    pub fn uniform(gutter: Rel<Abs>) -> Self {
        Self(smallvec![gutter])
    }

    /// The size of gutter `i` (after column `i`), relative to `base`; the last
    /// given size repeats.
    pub fn get(&self, i: usize, base: Abs) -> Abs {
        self.0
            .get(i)
            .or(self.0.last())
            .map_or(Abs::zero(), |g| g.relative_to(base))
    }
}

/// Forces a column break.
///
/// The function will behave like a @pagebreak[page break] when used in a single
/// column layout or the last column on a page. Otherwise, content after the
/// column break will be placed in the next column.
///
/// = Example <example>
/// ```example
/// #set page(columns: 2)
/// Preliminary findings from our
/// ongoing research project have
/// revealed a hitherto unknown
/// phenomenon of extraordinary
/// significance.
///
/// #colbreak()
/// Through rigorous experimentation
/// and analysis, we have discovered
/// a hitherto uncharacterized process
/// that defies our current
/// understanding of the fundamental
/// laws of nature.
/// ```
#[elem(title = "Column Break", since = "forever")]
pub struct ColbreakElem {
    /// If `{true}`, the column break is skipped if the current column is
    /// already empty.
    #[default(false)]
    pub weak: bool,
}

/// Where footnotes are placed in balanced columns.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Hash, Cast)]
pub enum BalanceNotes {
    /// Under the balanced text (the default).
    #[default]
    Raise,
    /// At the foot of the full region.
    Foot,
}

/// Balances the columns of the current page or container up to this point.
///
/// The content of the current page's columns (or the columns of the enclosing
/// @columns container) up to the marker is laid out into columns of equal,
/// minimal height. Each column holds its share of the text together with the
/// footnote entries of its markers, so text plus notes end level in all
/// columns. The rest of the page stays empty: content after the marker
/// continues on the next page (or, in a container, below the container, which
/// shrinks to the balanced height).
///
/// Parent-scoped floats that were already placed on the page, such as a title
/// spanning both columns, stay where they are; only the columns below them are
/// balanced. Footnote rules still hold: an entry is placed in the same column
/// as its marker, and widow/orphan prevention and sticky blocks are respected.
/// If no height shorter than the full column height fits the content, the
/// columns are laid out as without balancing.
///
/// In a single-column layout, this does nothing.
///
/// = Example <example>
/// ```example
/// #set page(columns: 2, height: 120pt)
/// Preliminary findings from our
/// ongoing research project have
/// revealed a hitherto unknown
/// phenomenon of extraordinary
/// significance.
/// #colbalance()
/// ```
/// Where a balanced page's footnotes stand, see [`ColbalanceElem::notes`].
#[elem(title = "Column Balance")]
pub struct ColbalanceElem {
    /// If set, the balanced column height is a whole multiple of this length,
    /// measured from the top of the columns. With a fixed line pitch, setting
    /// this to the pitch keeps the baselines of a balanced page on the same
    /// grid as those of a full page.
    ///
    /// ```example
    /// #set page(columns: 2, height: 120pt)
    /// #set par(leading: 0.65em)
    /// #lorem(30)
    /// #colbalance(snap: 1.65em)
    /// ```
    pub snap: Option<Length>,

    /// Where the `snap` steps start: the balanced height is `offset` plus a
    /// whole multiple of `snap`. Useful when the columns start below a
    /// parent-scoped float, but their content should still keep a grid that
    /// is registered on the page.
    #[default(Length::zero())]
    pub offset: Length,

    /// If `{true}`, the footnote areas of the balanced columns start level:
    /// each column's separator stands where the column with the deepest
    /// notes has it, so a column with fewer notes ends them higher instead of
    /// at the foot of the balanced height. A height only fits if every
    /// column's content ends above that common level.
    #[default(false)]
    pub level_notes: bool,

    /// Where the balanced columns' footnotes stand. With `{"raise"}` (the
    /// default) each column's notes follow its text, at the foot of the
    /// balanced height (raised under the text where there is room). With
    /// `{"foot"}` only the text is balanced: the columns break as with
    /// `{"raise"}`, but every column's footnote area stands at the foot of the
    /// full region, its last entry where a full page puts it.
    #[default(BalanceNotes::Raise)]
    pub notes: BalanceNotes,
}
