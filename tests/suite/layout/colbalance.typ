// Tests for column balancing.

--- colbalance-text paged ---
// Plain text: both columns end level, the rest of the page stays empty.
#set page(width: 200pt, height: 200pt, columns: 2)
#set par(justify: true)
#lorem(60)
#colbalance()

--- colbalance-snap paged ---
// The balanced height is a whole number of line pitches.
#set page(width: 200pt, height: 200pt, columns: 2)
#set text(top-edge: 8pt, bottom-edge: -3pt)
#set par(leading: 2pt, spacing: 2pt)
#lorem(50)
#colbalance(snap: 13pt)

--- colbalance-footnotes paged ---
// Footnote entries stay in their markers' columns. Balancing moves text, and
// with it a marker, from the first column into the second, so its entry
// moves along.
#set page(width: 220pt, height: 260pt, columns: 2)
#set text(size: 8pt)
#lorem(25)#footnote[First note, in the first column.]
#lorem(30)#footnote[Second note, which moves to the second column with its
  marker when the columns are balanced.]
#lorem(20)#footnote[Third note.]
#colbalance()

#context {
  for entry in query(footnote.entry) {
    assert.eq(entry.location().page(), entry.note.location().page())
  }
}

--- colbalance-below-float paged ---
// A parent-scoped float (a title) stays at the top of the page; only the
// columns below it are balanced.
#set page(width: 220pt, height: 220pt, columns: 2)
#place(top, scope: "parent", float: true, block(
  width: 100%, inset: 6pt, fill: aqua, align(center)[*A Title*],
))
#lorem(55)
#colbalance()

--- colbalance-mid-page paged ---
// A balance point after a page of text: the second page is balanced, the
// text after the page break starts on a fresh page.
#set page(width: 200pt, height: 160pt, columns: 2)
#lorem(110)
#colbalance()
#pagebreak(weak: true)
Next work.

--- colbalance-unbalanceable paged ---
// An unbreakable block taller than half the content can't be split: the
// columns are as short as the block allows. A block that only fits at full
// height leaves the layout as without balancing.
#set page(width: 200pt, height: 160pt, columns: 2)
#set block(breakable: false)
#block(height: 100pt, width: 100%, fill: aqua)
#lorem(8)
#colbalance()
#pagebreak()
#block(height: 100%, width: 100%, fill: aqua)
#block(height: 40pt, width: 100%, fill: teal)
#colbalance()

--- colbalance-single-column paged ---
// Without columns, the marker has no effect.
#set page(width: 120pt, height: 100pt)
#lorem(10)
#colbalance()
More text on the same page.

--- colbalance-container paged ---
// In a columns container, the container shrinks to the balanced height.
#set page(width: 200pt, height: 200pt)
#block(stroke: 0.5pt, columns(2, [#lorem(40) #colbalance()]))
After the columns.

--- colbalance-long-note-moves paged ---
// A long entry whose marker is on a paragraph's last lines: at heights where
// the entry would be split across columns, the layout is rejected; at a
// lower height the marker's lines move to the second column with the entry.
// The search must not step over that lower height.
#set page(width: 220pt, height: 300pt, columns: 2)
#set text(size: 8pt)
#lorem(70) And the end#footnote[#lorem(40)] of it.

#lorem(25)
#colbalance()

#context {
  for entry in query(footnote.entry) {
    assert.eq(entry.location().page(), entry.note.location().page())
  }
}
