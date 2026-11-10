--- par-hang-basic paged ---
// The first two lines are indented; the rest of the paragraph is broken with them.
#set page(width: 160pt, height: auto)
#set par(justify: true, hang: (lines: 2, indent: 30pt))
#place(dx: 0pt, dy: 0pt, rect(width: 26pt, height: 20pt, fill: aqua))
#lorem(40)

--- par-hang-all-lines paged ---
// A shape over every line breaks exactly as a narrower paragraph does.
#set page(width: 160pt, height: auto)
#set par(justify: true)
#block(width: 130pt, lorem(30))
#line(length: 100%)
#par(hang: (lines: 100, indent: 30pt), lorem(30))

--- par-hang-zero paged ---
// An empty shape changes nothing.
#set page(width: 160pt, height: auto)
#set par(justify: true)
#par(lorem(30))
#line(length: 100%)
#par(hang: (lines: 2, indent: 0pt), lorem(30))

--- par-hang-no-overhang paged ---
// Nothing hangs into the indent; the other lines hang as before.
#set page(width: 160pt, height: auto)
#set text(overhang: true)
#set par(justify: true, hang: (lines: 2, indent: 30pt))
“Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
“tempor incididunt ut labore et dolore magna aliqua. #linebreak(justify: true)
“Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.

--- par-hang-dict paged empty ---
#context test(par.hang, (lines: 0, indent: 0pt))
#set par(hang: (lines: 3, indent: 1em))
#context test(par.hang, (lines: 3, indent: 1em))

--- par-hang-soft-hyphen paged ---
// A soft hyphen is a costed hyphenation, and no break where hyphenation is off.
#set page(width: 100pt, height: auto)
#set par(justify: true)
so­journs so­journs so­journs so­journs so­journs so­journs
#set text(hyphenate: false)
so­journs so­journs so­journs so­journs so­journs so­journs

--- par-hang-contrast paged ---
// With a contrast cost, the shape's neighbouring lines are set with like gaps.
#set page(width: 160pt, height: auto)
#set par(justify: true, hang: (lines: 2, indent: 30pt))
#lorem(30)
#set text(costs: (contrast: 100%))
#lorem(30)

--- linebreak-overstretch paged ---
// Stretch past the justification limits can be made to cost more.
#set page(width: 120pt, height: auto)
#set text(hyphenate: false)
#set par(justify: true)
These things I have written to you by Crescens, whom up to the present time I have recommended unto you.
#set text(costs: (overstretch: 1000%))
These things I have written to you by Crescens, whom up to the present time I have recommended unto you.

--- linebreak-negative-spacing-kern paged ---
// Negative spacing is a kern: no break opportunity on either side.
#set page(width: 60pt, height: auto)
aaa bbb ccc#h(-0.1em)ddd eee

--- hyphenate-before-footnote paged ---
// A word directly followed by a footnote marker (kerned or not) still hyphenates.
#set page(width: 60pt, height: auto)
#set text(hyphenate: true)
aa bbb congregations#footnote[x] of the
aa bbb congregations#h(-0.015em)#footnote[y] of the

--- overhang-hanging-indent-first-line paged ---
// A hanging indent's first line starts at the margin: its opening quotation
// mark hangs there, as on any other line.
#set page(width: 160pt, height: auto)
#set text(overhang: true)
#set par(hanging-indent: 2em, justify: false)
“Let cares like a wild deluge come,

“And storms of sorrow fall,”

--- par-looseness paged ---
// A paragraph set a line longer, and a line shorter, than its best breaks.
#set page(width: 160pt, height: auto)
#set par(justify: true)
#lorem(40)
#line(length: 100%)
#par(looseness: 1, lorem(40))
#line(length: 100%)
#par(looseness: -1, lorem(40))
