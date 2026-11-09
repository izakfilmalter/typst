#!/usr/bin/env bash
# Build the typst-py wheel (Python binding, https://github.com/messense/typst-py)
# against this checkout and install it into .venv-py.
#
# typst-py v0.15.2 depends on typst 0.15.1 from crates.io; a [patch.crates-io]
# section redirects every engine crate it uses to this fork. The wheel gets the
# local version `0.15.2+lanes` so it is never mistaken for the PyPI release.
#
# Usage: tools/build-python-lanes.sh   (env: PROFILE=release|dev, JOBS=n)
set -euo pipefail
fork_dir=$(cd "$(dirname "$0")/.." && pwd)
binding_dir=${TYPST_PY_SOURCE:-"$fork_dir/target/typst-py"}
venv="$fork_dir/.venv-py"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=${JOBS:-6}
export CARGO_TARGET_DIR="$fork_dir/target/python"

if [[ ! -d "$binding_dir/.git" ]]; then
  git clone --branch v0.15.2 --depth 1 https://github.com/messense/typst-py "$binding_dir"
fi
[[ -x "$venv/bin/python" ]] || uv venv "$venv"
uv pip install --python "$venv/bin/python" 'maturin>=1.8,<2'

# Point the binding at the fork. Idempotent: rewrites the marked section.
"$venv/bin/python" - "$fork_dir" "$binding_dir" <<'PY'
import json, pathlib, re, sys, tomllib

fork, binding = map(pathlib.Path, sys.argv[1:])
manifest = binding / "Cargo.toml"
marker = "\n# --- footnote-lanes fork ---\n"
text = manifest.read_text().split(marker)[0]
text = re.sub(r'(?m)^version = "0\.15\.2"$', 'version = "0.15.2+lanes"', text, count=1)

# Only patch crates that the binding actually depends on (avoids
# "patch was not used" warnings for the CLI, docs, etc.).
lock = tomllib.loads((binding / "Cargo.lock").read_text())
used = {p["name"] for p in lock["package"]}
lines = []
for crate in sorted((fork / "crates").iterdir()):
    name = tomllib.loads((crate / "Cargo.toml").read_text())["package"]["name"]
    if name in used:
        lines.append(f"{name} = {{ path = {json.dumps(str(crate))} }}")
# The default clone lives under the fork's target/, inside its Cargo
# workspace; an empty [workspace] table keeps the binding standalone.
workspace = "" if re.search(r"(?m)^\[workspace\]", text) else "[workspace]\n\n"
manifest.write_text(
    text + marker + workspace + "[patch.crates-io]\n" + "\n".join(lines) + "\n"
)
PY

cd "$binding_dir"
rm -f "$fork_dir"/dist/typst-*.whl
"$venv/bin/maturin" build --profile "${PROFILE:-release}" \
  --interpreter "$venv/bin/python" --out "$fork_dir/dist"
uv pip install --python "$venv/bin/python" --reinstall "$fork_dir"/dist/typst-*.whl

"$venv/bin/python" "$fork_dir/tools/smoke-python-lanes.py"
