// Tests for the continuation cue of split footnote entries.

--- footnote-continuation-content paged ---
// A content cue stands in the number's place on the continued part.
#set page(width: 160pt, height: 100pt)
#set footnote.entry(continuation: [(cont.)#h(0.3em)])
#show footnote.entry: it => par(hanging-indent: 2.4em, it)
A#footnote[#lines(8)] B#footnote[Short.]

--- footnote-continuation-func paged ---
// A function cue receives the footnote and runs in its context, here giving
// the number in parentheses. Raised text on the continued line doesn't move
// the cue off the line's baseline.
#set page(width: 160pt, height: 100pt)
#set footnote(numbering: "a")
#set footnote.entry(
  continuation: note => {
    assert(note.func() == footnote)
    [(#counter(footnote).display(note.numbering))#h(0.3em)]
  },
)
#show footnote.entry: it => par(hanging-indent: 1.4em, it)
One#footnote[Short.] Two#footnote[#lines(5) \ Six#super[x] and #lines(2)]

--- footnote-continuation-many-parts paged ---
// Every continued part gets a cue; the entry's first part keeps its number.
#set page(width: 160pt, height: 70pt)
#set footnote.entry(continuation: [--#h(0.2em)])
#show footnote.entry: it => par(hanging-indent: 1.4em, it)
X#footnote[#lines(10)]
#context {
  let entries = query(footnote.entry)
  assert.eq(entries.len(), 1)
}

--- footnote-continuation-columns paged ---
// The continued part in the next column, in a lane, gets the cue too.
#set page(width: 260pt, height: 110pt, columns: 2)
#set footnote.entry(
  lanes: (1fr, 1fr),
  lane-gutter: 6pt,
  continuation: text(fill: red)[›#h(0.2em)],
)
#show footnote.entry: it => par(hanging-indent: 1em, it)
L#footnote(lane: 0)[#lines(9)] R#footnote(lane: 1)[Right.]

--- footnote-continuation-none paged ---
// Without a cue (the default), the continued part starts plainly.
#set page(width: 160pt, height: 100pt)
#show footnote.entry: it => par(hanging-indent: 2.4em, it)
A#footnote[#lines(8)] B#footnote[Short.]

--- footnote-continuation-rtl paged ---
// In right-to-left text, the cue hangs to the right of the continued line.
#set page(width: 160pt, height: 100pt)
#set text(dir: rtl)
#set footnote.entry(continuation: [(+)#h(0.2em)])
#show footnote.entry: it => par(hanging-indent: 1.6em, it)
A#footnote[#lines(8)] B#footnote[Short.]

--- footnote-continuation-unsplit paged ---
// Entries that don't split never show the cue.
#set page(width: 160pt, height: 100pt)
#set footnote.entry(continuation: [CONT])
A#footnote[One.] B#footnote[Two.]

--- footnote-continuation-wrong-type eval ---
// Error: 35-37 expected content, function, or none, found integer
#set footnote.entry(continuation: 12)
