# Vendored comemo 0.5.1 (fork)

comemo and comemo-macros 0.5.1 from crates.io, patched for theology-books' compile server
(`[patch.crates-io] comemo = { git = …typst, branch = … }` picks these up from this repo):

- every memoized function's cache is registered by name (`module::fn`): `comemo::stats()`
  (entries, call-tree nodes, hits and misses since the last call) and, with
  `comemo::set_explain(true)`, why lookups miss (a new key, or the first tracked call whose
  return differs);
- `comemo::evict_with(default, policy)`: a maximum age per cache, `usize::MAX` keeps it;
- `comemo::set_nesting(true)`: each result remembers the results it was made of (across
  `Engine::parallelize` too: `comemo::frame` / `in_frame`), and a hit marks them used, so
  the parts of a result that keeps hitting are not evicted as unused (`new_epoch` between
  evictions of the kept caches).

Off by default: a build that sets none of these behaves as upstream comemo.
