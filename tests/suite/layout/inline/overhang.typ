// Test micro-typographical shenanigans.

--- overhang paged ---
// Test hanging punctuation.
// TODO: This test was broken at some point.
#set page(width: 130pt, margin: 15pt)
#set par(justify: true, linebreaks: "simple")
#set text(size: 9pt)
#rect(inset: 0pt, fill: rgb(0, 0, 0, 0), width: 100%)[
  This is a little bit of text that builds up to
  hang-ing hyphens and dash---es and then, you know,
  some punctuation in the margin.
]

// Test hanging punctuation with RTL.
#set text(lang: "he", font: ("PT Sans", "Noto Serif Hebrew"))
בנייה נכונה של משפטים ארוכים דורשת ידע בשפה. אז בואו נדבר על מזג האוויר.

--- overhang-lone paged ---
// Test that lone punctuation doesn't overhang into the margin.
#set page(margin: 0pt)
#set align(end)
#set text(dir: rtl)
:

--- overhang-after-metadata paged ---
// A closing quotation mark or stop hangs even when a zero-width marker
// (metadata) follows it before the line's trailing space: the space, trimmed
// at the break, leaves a glyph-less text item that hanging looks past.
#set page(width: 120pt, height: auto, margin: 10pt)
#set par(justify: true)
#set text(overhang: true, hyphenate: false)
#rect(inset: 0pt, width: 100%)[
  He said, “the end of all thereof.”#metadata(1)<m> And so it is with everything.#metadata(2)<m> Then we go on.
]
#rect(inset: 0pt, width: 100%)[
  He said, “the end of all thereof.” And so it is with everything. Then we go on.
]
