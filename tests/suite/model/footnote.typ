
--- footnote-widow-moves-with-group paged ---
// A paragraph's last line whose footnote doesn't fit moves on with the line
// before it (widow prevention), so it never stands alone at the next top.
#set page(height: 120pt, width: 120pt)
#set par(justify: true)
#lorem(28) This is the end of it.#footnote[A note long enough that it needs several lines of room at the foot of the page, more than is left.]
