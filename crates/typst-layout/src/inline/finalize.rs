use typst_library::introspection::SplitLocator;
use typst_utils::Numeric;

use super::*;

/// Turns the selected lines into frames.
#[typst_macros::time]
pub fn finalize(
    engine: &mut Engine,
    p: &Preparation,
    lines: &[Line],
    region: Size,
    expand: bool,
    locator: &mut SplitLocator<'_>,
) -> SourceResult<Fragment> {
    // Determine the resulting width: Full width of the region if we should
    // expand or there's fractional spacing, fit-to-width otherwise.
    let width = if !region.x.is_finite()
        || (!expand && lines.iter().all(|line| line.fr().is_zero()))
    {
        region.x.min(
            p.config.hanging_indent
                + lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| line.width + p.config.hang_at(i))
                    .max()
                    .unwrap_or_default(),
        )
    } else {
        region.x
    };

    // Stack the lines into one frame per region.
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            // (the first line's anchor box stays at the nominal indent)
            let anchor = if i == 0 { -p.config.hang_shift.get() } else { Abs::zero() };
            commit(engine, p, line, width, region.y, p.config.hang_at(i), anchor, locator)
        })
        .collect::<SourceResult<_>>()
        .map(Fragment::frames)
}
