// Footnote lanes demo. Compile with the `footnote-lanes` branch of the fork:
//   typst compile demo/lanes.typ demo/lanes.pdf
//
// A single-column page holds a two-column synopsis grid. Each recension's
// notes sit at the foot of the page that carries their marker, beneath their
// own column. Grid rows break across pages.
#set page(
  width: 400pt, height: 420pt, margin: (x: 30pt, y: 36pt), columns: 1,
  header: context {
    set text(size: 8pt)
    grid(columns: (1fr, 1fr), column-gutter: 18pt,
      align(center, smallcaps[Shorter recension]),
      align(center, smallcaps[Longer recension]))
  },
  footer: context align(center, text(size: 8pt, counter(page).display())),
)
#set text(font: "Libertinus Serif", size: 10pt)
#set par(leading: 3pt, spacing: 5pt, justify: true)
#set footnote(numbering: "a")
#set footnote.entry(
  lanes: (1fr, 1fr), lane-gutter: 18pt,
  separator: line(length: 30%, stroke: 0.4pt),
  clearance: 8pt, gap: 4pt, indent: 0pt,
)
#show footnote.entry: set text(size: 8pt)
#show footnote.entry: set par(leading: 2.5pt)

#align(center, strong[Synoptic texts with page-foot lanes])

#let recension(side, count, every) = {
  // Each cell selects its lane once; every note inside follows.
  set footnote(lane: if side == "L" { 0 } else { 1 })
  for i in range(1, count + 1) {
    let id = side + str(i)
    par[
      *#id.*
      #if side == "L" {
        [The shorter witness preserves these words, offering a concise account
         of the teaching and its reception among the hearers.]
      } else {
        [The longer witness expands this passage with an account of the
         teaching, the hearers who gathered, and the questions they brought
         concerning the words handed down to them.]
      }
      #if calc.rem(i, every) == 1 {
        footnote[
          #id. This reading belongs to the
          #if side == "L" [shorter] else [longer] recension.
        ]
      }
    ]
  }
}

// Each chapter is one grid row. The rows deliberately break across pages:
// the shorter cell ends first, while the longer one and its notes continue.
#for (ch, short, long) in ((1, 12, 20), (2, 4, 7), (3, 9, 14)) {
  block(above: 12pt, below: 6pt, width: 100%,
    align(center, smallcaps[Chapter #ch]))
  grid(
    columns: (1fr, 1fr), column-gutter: 18pt,
    recension("L", short, 3), recension("R", long, 2),
  )
}

// Ordinary entries stay full width, between the separator and the lanes.
The synopsis is complete.#footnote[An ordinary full-width editorial note.]
