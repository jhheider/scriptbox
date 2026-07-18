# scriptbox compatibility suite

A broad, real-world cross-shell compatibility regimen for `scriptbox`, separate
from the unit/e2e tests in `tests/`. It runs a matrix of shell idioms - and a
read-only pass over real public installer scripts - through `scriptbox` across
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

- **Transparency** - `scriptbox <shell> x.sh` must produce the *same* stdout+exit
  as `<shell> x.sh`. scriptbox is supposed to be invisible; a per-shell divergence
  is a bug (or a case that needs a documented flag). Compared per-shell, so
  inherent cross-shell differences (array indexing, etc.) aren't false positives.
  Idioms: command/process substitution, subshells, background jobs + `wait`,
  heredocs, `EXIT` traps, `set -eu` + pipes, `exec` replacement, arrays, a
  4000-line script (to stress the memfd/unlink-then-read path beyond a toy), and
  stdin `read` (the script's own interactive input must still work through the
  frozen fd).
- **Insulation** - the whole point: a script that appends to itself mid-run must
  be frozen under scriptbox (the tampered line must not run), while a plain shell
  is vulnerable. Checked on all four shells.
- **`$0` handling (`--argv0`)** - reports what `$0` resolves to per shell in
  default vs `source` mode, with and without a shebang (see the note below).
- **No added linter findings** - for each idiom, `shellcheck` the original vs the
  bytes scriptbox actually serves (`scriptbox emit <shell> x.sh`), and assert the
  served copy introduces **no shellcheck code the original didn't already have**.
  It's fine for a script to have findings; scriptbox just must not add any. This
  is why the `$0` reset is joined onto the first body line rather than replacing
  the shebang (which would drop the shell dialect and add SC2148).
- **`--subscripts`** (full-featured build only) - confirms `report` detects a
  `source`d sibling; freezing is exercised by the transparency run.
- **Real public installers** - `rustup`, `nvm`, `docker`, fetched fresh, run
  through `scriptbox hash` only (read-only; installers are **not** executed -
  side effects, and it dogfoods the `pin`/`hash` path against real gnarly shell).

## Flag annotations (what each case needs, and why)

| Case | Flag needed | Why |
|---|---|---|
| Most idioms | none | scriptbox is transparent by default |
| `$0` on `bash`>=5 / `zsh` | none | rewrite swaps the shebang line, or prepends when there's none |
| `$0` on `dash` / `ksh` | `--argv0 source` | no in-run `$0` rewrite mechanism; dot-source sets a real `$0` |
| `$0` on `zsh` | default (not `source`) | zsh's dot-source resets `$0` to the fd path, so `source` mode is worse there |
| `source`ing a sibling by path | none (uses `$SCRIPTBOX_SOURCE`) | the fd path can't be `dirname`'d; `$SCRIPTBOX_SOURCE` is the real path |
| freezing a `source`d child | `--subscripts=freeze` | otherwise the child is a live re-read, one level down |

## Fixed: `$0` rewrite is line-faithful and lint-clean (was issue #1)

The suite caught this: the `--argv0 rewrite` used to **swap the shebang line** for
the injected `$0` reset. That dropped the shebang (so `shellcheck` lost the shell
dialect and added SC2148), and a shebang-less script got no rewrite at all - `$0`
stayed the fd path even on `bash`>=5 / `zsh`.

Now (`src/interpreter.rs`) the `$0` reset is **joined onto the first body line with
`;`** - after the shebang if there is one, else at the start. This adds no line, so
error line numbers stay exact, and the shebang stays on line 1, so the served copy
is lint-clean (the shellcheck check above proves it adds no findings). bash>=5 /
zsh resolve the real path with or without a shebang; macOS bash 3.2 has no
`BASH_ARGV0`; dash/ksh still need `--argv0 source`. The checksum gate is unaffected
- it runs over the pre-rewrite bytes, so a pin verifies the file on disk shebang or
not. Inspect the served bytes for any script with `scriptbox emit <shell> x.sh`.

## The macOS gap

Docker is Linux, so this covers only scriptbox's **Linux** frozen-copy path (a
sealed `memfd`, exec'd as `/proc/self/fd/N`). macOS uses a **different** path (a
written-then-unlinked private temp, exec'd as `/dev/fd/N`) that Docker cannot
exercise. Run `sh compat/run.sh <scriptbox>` on real macOS separately for it - the
harness is the same. (`fish`/`nushell` are intentionally out of scope; scriptbox
does not wrap them.)
