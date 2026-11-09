// Tests for footnote lanes: entries laid out side by side at the page foot.

--- footnote-lanes-unequal paged ---
// Two lanes with unequal note counts share one separator; the area is as
// tall as the taller lane and both lanes are top-aligned.
#set page(width: 240pt, height: 150pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 20pt)
#grid(
  columns: (1fr, 1fr),
  column-gutter: 20pt,
  [
    #set footnote(lane: 0)
    Left#footnote[Left one.]
    more#footnote[Left two.]
    last#footnote[Left three.]
  ],
  [
    #set footnote(lane: 1)
    Right#footnote[Right only, but long enough to wrap.]
  ],
)

--- footnote-lanes-grid-break paged ---
// A single grid row breaks across pages. Each entry stays on the page of its
// marker, in the lane of its column.
#set page(width: 240pt, height: 130pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 12pt)
#grid(
  columns: (1fr, 1fr),
  column-gutter: 12pt,
  range(8).map(i => [Left #i#footnote(lane: 0)[L#i.] \ ]).join(),
  range(12).map(i => [Right #i#footnote(lane: 1)[R#i.] \ ]).join(),
)

#context {
  let notes = query(footnote)
  let pages = notes.map(n => n.location().page()).dedup()
  assert(pages.len() >= 2)
  for entry in query(footnote.entry) {
    assert.eq(entry.location().page(), entry.note.location().page())
  }
  assert(notes.filter(n => n.lane == 0).any(n => n.location().page() > 1))
}

--- footnote-lanes-mixed paged ---
// Entries without a lane span the full width, between the separator and the
// lanes, regardless of marker order.
#set page(width: 240pt, height: 150pt)
#set footnote.entry(lanes: (100pt, 1fr), lane-gutter: 20pt)
Left#footnote(lane: 0)[Left lane.]
Full#footnote[Full width, above both lanes, even though its marker follows.]
Right#footnote(lane: 1)[Right lane.]

--- footnote-lanes-widths paged ---
// Lengths, ratios and fractions mix like grid columns.
#set page(width: 240pt, height: 130pt)
#set footnote.entry(lanes: (50pt, 25%, 1fr), lane-gutter: 10pt)
A#footnote(lane: 0)[First lane.]
B#footnote(lane: 1)[Second lane.]
C#footnote(lane: 2)[Third lane, which takes the rest.]

--- footnote-lanes-split paged ---
// Long entries split per lane. A lane's spill only delays later entries in
// the same lane.
#set page(width: 240pt, height: 110pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 20pt)
Left#footnote(lane: 0)[#lines(12)]
Right#footnote(lane: 1)[R]
Next left#footnote(lane: 0)[Left after spill.]
Next right#footnote(lane: 1)[Right, still on the first page.]

--- footnote-lanes-migration paged ---
// An entry that doesn't fit at all moves to the next page with its marker.
#set page(width: 240pt, height: 100pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 20pt)
#lines(5)
Move#footnote(lane: 1)[Must stay with its marker.]
#context for entry in query(footnote.entry) {
  assert.eq(entry.location().page(), entry.note.location().page())
}

--- footnote-lanes-nested paged ---
// Nested footnotes and references choose their own lane.
#set page(width: 240pt, height: 150pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 20pt)
Left#footnote(lane: 0)[Outer#footnote(lane: 1)[Nested, in the right lane.]] <left>
Ref#footnote(<left>, lane: 1)
Right#footnote(lane: 1)[Right.]

--- footnote-lanes-columns paged ---
// In a multi-column page, lanes are local to each page column.
#set page(width: 300pt, height: 120pt, columns: 2)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 8pt)
A#footnote(lane: 0)[One.] B#footnote(lane: 1)[Two.]
#colbreak()
C#footnote(lane: 1)[Three.]

--- footnote-lanes-no-lanes paged ---
// Without lane configuration, ordinary footnotes are unaffected.
#set page(width: 240pt, height: 100pt)
A#footnote[One.] B#footnote(lane: none)[Two.]

--- footnote-lanes-unconfigured paged ---
// Error: 2-33 footnote lane 0 does not exist
// Hint: 2-33 0 lanes are configured with `footnote.entry(lanes: ..)`
#footnote(lane: 0)[Unconfigured]

--- footnote-lanes-out-of-bounds paged ---
#set footnote.entry(lanes: (1fr,))
// Error: 2-33 footnote lane 1 does not exist
// Hint: 2-33 1 lanes are configured with `footnote.entry(lanes: ..)`
#footnote(lane: 1)[Out of range]

--- footnote-lanes-negative-index eval ---
// Error: 17-19 number must be at least zero
#footnote(lane: -1)[Negative]

--- footnote-lanes-zero-width paged ---
#set footnote.entry(lanes: (0pt, 1fr))
// Error: 2-26 footnote lane 0 has no width
#footnote(lane: 0)[Empty]

--- footnote-lanes-negative-gutter paged ---
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: -1pt)
// Error: 2-35 footnote lane gutter must not be negative
#footnote(lane: 0)[Invalid gutter]

--- footnote-lanes-overflow paged ---
#set footnote.entry(lanes: (50%, 50%), lane-gutter: 10pt)
// Error: 2-29 footnote lanes and gutters are wider than the footnote area
#footnote(lane: 0)[Overflow]
