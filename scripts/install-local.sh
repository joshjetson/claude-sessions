#!/usr/bin/env bash
#
# Install this checkout over whatever `claude-sessions` is on your PATH, and
# hand the daemon's port to the new build.
#
#   scripts/install-local.sh            install the working tree as it stands
#   scripts/install-local.sh main       check out and fast-forward main first
#   scripts/install-local.sh some/branch
#
# For developing on this repository. If you just want to run the dashboard,
# `cargo install claude-sessions` from crates.io is the supported route — see
# the README.
#
# What it does NOT do: launch the dashboard. That takes over the terminal, and a
# step that hijacks your shell does not belong at the end of an install script.
set -euo pipefail

REF="${1:-}"

# Run from anywhere inside the repo; everything below is relative to its root.
ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "error: run this from inside the repository." >&2
  exit 1
}
cd "$ROOT"

command -v cargo >/dev/null || {
  echo "error: cargo is not on PATH. Install Rust from https://rustup.rs" >&2
  exit 1
}

# --- 1. pick the code -------------------------------------------------------
dirty() { [ -n "$(git status --porcelain)" ]; }

if [ -n "$REF" ]; then
  # Switching refs would discard or carry uncommitted work depending on the
  # overlap, and neither is a decision a script should make for you.
  if dirty; then
    echo "error: uncommitted changes — commit or stash before switching to '$REF'." >&2
    git status --short >&2
    exit 1
  fi
  echo "==> checking out $REF"
  git checkout "$REF"
  # --ff-only: a merge here would be a surprise, and a conflict mid-install is
  # worse than a refusal.
  git pull --ff-only
elif dirty; then
  echo "note: installing the working tree, which has uncommitted changes:"
  git status --short | sed 's/^/      /'
fi

DESC="$(git rev-parse --short HEAD) on $(git rev-parse --abbrev-ref HEAD)"
dirty && DESC="$DESC (+ uncommitted changes)"
echo "==> installing $DESC"

# --- 2. build and install ---------------------------------------------------
# --locked pins the dependency versions to Cargo.lock, so this build is the one
# the tests ran against. (The README's note that `cargo install` never needs
# --locked is about installing from crates.io, where a fresh resolve is the
# thing being guaranteed. Different question.)
cargo install --path . --locked

# --- 3. check PATH actually finds it ----------------------------------------
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/claude-sessions"
RESOLVED="$(command -v claude-sessions || true)"

if [ "$RESOLVED" != "$CARGO_BIN" ]; then
  echo
  echo "warning: PATH finds a different claude-sessions first:"
  echo "           $RESOLVED"
  echo "         this build installed to:"
  echo "           $CARGO_BIN"
  echo "         put ${CARGO_HOME:-\$HOME/.cargo}/bin ahead of it, or call the"
  echo "         absolute path. A shell opened before now may also have cached"
  echo "         the old location — 'hash -r' clears that."
fi

# --- 4. hand the port over --------------------------------------------------
# A running daemon keeps executing the code it started with: replacing the file
# on disk does not touch a live process. Without this you get the new dashboard
# talking to the old daemon, which reports as a version mismatch rather than as
# anything obviously wrong.
echo "==> stopping any running daemon so the new build owns the port"
"$CARGO_BIN" daemon stop 2>/dev/null || echo "   (none was running)"

echo
echo "==> installed: $("$CARGO_BIN" --version)"
echo "    from:      $DESC"
echo
echo "Start it with:  claude-sessions"
echo "Check it with:  claude-sessions doctor"
