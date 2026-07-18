# scriptbox compatibility suite

A broad, real-world cross-shell compatibility regimen for `scriptbox`, separate
from the unit/e2e tests in `tests/`. It runs a matrix of shell idioms — and a
read-only pass over real public installer scripts — through `scriptbox` across
all four shells it supports (`bash`, `zsh`, `dash`, `ksh`), reproducibly in
Docker.

## Run it

```sh
# Linux memfd path, all four shells, reproducible:
docker build -t scriptbox-compat -f compat/Dockerfile .
docker run --rm scriptbox-compat
# or:  just compat

# Locally against a built binary (on macOS this exercises the unlink-then-read path):
cargo build --release --features subscripts
sh compat/run.sh target/release/scriptbox
```

Exit status is non-zero if any transparency divergence or insulation failure is
found.

## What it checks

- **Transparency** — `scriptbox <shell> x.sh` must produce the *same* stdout+exit
  as `<shell> x.sh`. scriptbox is supposed to be invisible; a per-shell divergence
  is a bug (or a case that needs a documented flag). Compared per-shell, so
  inherent cross-shell differences (array indexing, etc.) aren't false positives.
  Idioms: command/process substitution, subshells, background jobs + `wait`,
  heredocs, `EXIT` traps, `set -eu` + pipes, `exec` replacement, arrays, a
  4000-line script (to stress the memfd/unlink-then-read path beyond a toy), and
  stdin `read` (the script's own interactive input must still work through the
  frozen fd).
- **Insulation** — the whole point: a script that appends to itself mid-run must
  be frozen under scriptbox (the tampered line must not run), while a plain shell
  is vulnerable. Checked on all four shells.
- **`$0` handling (`--argv0`)** — reports what `$0` resolves to per shell in
  default vs `source` mode, with and without a shebang (see the finding below).
- **`--subscripts`** (full-featured build only) — confirms `report` detects a
  `source`d sibling; freezing is exercised by the transparency run.
- **Real public installers** — `rustup`, `nvm`, `docker`, fetched fresh, run
  through `scriptbox hash` only (read-only; installers are **not** executed —
  side effects, and it dogfoods the `pin`/`hash` path against real gnarly shell).

## Flag annotations (what each case needs, and why)

| Case | Flag needed | Why |
|---|---|---|
| Most idioms | none | scriptbox is transparent by default |
| `$0` on `bash`>=5 / `zsh` | none, **but a shebang must be present** | rewrite swaps the shebang line (see finding) |
| `$0` on `dash` / `ksh` | `--argv0 source` | no in-run `$0` rewrite mechanism; dot-source sets a real `$0` |
| `$0` on `zsh` | default (not `source`) | zsh's dot-source resets `$0` to the fd path, so `source` mode is worse there |
| `source`ing a sibling by path | none (uses `$SCRIPTBOX_SOURCE`) | the fd path can't be `dirname`'d; `$SCRIPTBOX_SOURCE` is the real path |
| freezing a `source`d child | `--subscripts=freeze` | otherwise the child is a live re-read, one level down |

## Finding: `$0` rewrite requires a shebang line

The default `--argv0 rewrite` works by **swapping the script's shebang line** for
an injected `$0` prologue (this preserves line numbers). Consequence: a script
with **no shebang**, run via an explicit `scriptbox <shell> script`, never gets
the rewrite — `$0` stays the fd path (`/proc/self/fd/N` on Linux, `/dev/fd/N` on
macOS) even on `bash`>=5 / `zsh`, where the rewrite would otherwise succeed.

This does *not* affect the shebang-launch path (`#!/usr/bin/env -S scriptbox
bash`), because that line *is* a shebang. It only bites explicit invocation of a
shebang-less file. Reproduced on Linux (bash 5) and macOS. Filed upstream; the
call is fix (prepend when no shebang, accepting a 1-line offset) vs. document.

## The macOS gap

Docker is Linux, so this covers only scriptbox's **Linux** frozen-copy path (a
sealed `memfd`, exec'd as `/proc/self/fd/N`). macOS uses a **different** path (a
written-then-unlinked private temp, exec'd as `/dev/fd/N`) that Docker cannot
exercise. Run `sh compat/run.sh <scriptbox>` on real macOS separately for it — the
harness is the same. (`fish`/`nushell` are intentionally out of scope; scriptbox
does not wrap them.)
