"""Smoke test: the installed `typst` wheel is the footnote-lanes build."""

import pathlib
import sys
import tempfile

import typst

LANES = b"""
#set page(width: 240pt, height: 150pt)
#set footnote.entry(lanes: (1fr, 1fr), lane-gutter: 12pt)
#grid(columns: (1fr, 1fr), column-gutter: 12pt,
  [#set footnote(lane: 0)
   Left#footnote[Left one.] more#footnote[Left two.]],
  [#set footnote(lane: 1)
   Right#footnote[Right one.]],
)
#context {
  let entries = query(footnote.entry)
  assert.eq(entries.len(), 3)
  assert.eq(query(footnote).map(n => n.lane), (0, 0, 1))
}
"""

# A stock typst build rejects the unknown `lane` argument.
pdf = typst.compile(LANES)
assert pdf.startswith(b"%PDF-"), "expected PDF output"

# Out-of-range lanes must be reported as a Typst error.
try:
    typst.compile(b"#footnote(lane: 3)[x]")
except Exception as err:  # typst raises TypstError
    assert "footnote lane 3 does not exist" in str(err), err
else:
    sys.exit("expected an error for an unconfigured lane")

demo = pathlib.Path(__file__).resolve().parent.parent / "demo" / "lanes.typ"
if demo.exists():
    with tempfile.TemporaryDirectory() as tmp:
        out = pathlib.Path(tmp) / "lanes.pdf"
        typst.compile(str(demo), output=str(out))
        assert out.read_bytes().startswith(b"%PDF-")

print(f"typst-py {typst.__version__ if hasattr(typst, '__version__') else ''} footnote lanes: OK")
