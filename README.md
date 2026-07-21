# `scriptbox`

[![CI](https://github.com/jhheider/scriptbox/actions/workflows/ci.yml/badge.svg)](https://github.com/jhheider/scriptbox/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Shells read your script *as they run it*, a line at a time straight off disk.
So if the file changes mid-run, the shell reads on from its byte offset into the
new bytes and runs whatever now lands there:

```console
$ cat job.sh
#!/bin/bash
sleep 1
echo "step 1: deploy to staging"

$ ./job.sh &                                    # while it sleeps...
$ echo 'echo "step 2: DELETE PROD"' >> job.sh   # ...something appends a line
$ wait
step 1: deploy to staging
step 2: DELETE PROD                             # you never wrote step 2. it ran anyway.
```

`bash`, `zsh`, `dash`, and `ksh` all do this; POSIX practically requires it.
Everything is fine until the one time it isn't. (Shells that parse the whole
file first (fish, nushell, and every non-shell like python/ruby/node) are
already immune, so scriptbox doesn't wrap them.)

Change one line, the shebang, and the file is frozen the moment it starts:

```console
$ head -1 job.sh
#!/usr/bin/env -S scriptbox bash

$ ./job.sh &
$ echo 'echo "step 2: DELETE PROD"' >> job.sh   # same edit, mid-run
$ wait
step 1: deploy to staging                       # ...and step 2 never runs
```

`scriptbox` reads the whole script into an immutable copy the instant you invoke
it, then hands *that* to the real `bash`. Edit the original all you like; the run
you launched finishes exactly as it was written.

It's a shebang loader, not a shell replacement. `./job.sh` still works, with the
same arguments, stdin, exit code, and correct line numbers in errors.

## Install

```sh
brew install jhheider/tap/scriptbox      # Homebrew (macOS + Linux)
```

```sh
curl -fsSL https://heider.cc/scriptbox.sh | sh      # prebuilt binary -> ~/.local/bin
```

Or grab a binary from [the latest release](https://github.com/jhheider/scriptbox/releases/latest)
(`scriptbox-{linux,macos}-{aarch64,x86_64}.tar.gz`), or build it:

```sh
cargo install --git https://github.com/jhheider/scriptbox
```

## What it does

1. **Freezes the script.** It reads the whole file once into a copy nothing can
   reach to change, then runs the interpreter against *that*, so a mid-run edit
   to the original can't rewrite what's already executing.
2. **Pins it, if you want.** Add a `sha256` checksum and scriptbox refuses to run
   on any drift. `scriptbox pin ./job.sh` prints the line to paste; `scriptbox
   hash ./job.sh` prints just the digest.
3. **Keeps your script locatable.** `$SCRIPTBOX_SOURCE` always points at the real
   file, and `$0` is rewritten back to it where the shell allows.

## Usage

Two ways to name the interpreter: on the shebang line, or in a PEP-723-style
`# /// scriptbox` block that still runs under a plain shell when scriptbox isn't
installed (it's all `#` comments):

```sh
#!/usr/bin/env -S scriptbox bash
```

```sh
#!/usr/bin/env scriptbox
# /// scriptbox
# interpreter = "bash"
# checksum = "sha256:1f0c..."
# ///
```

Or explicitly: `scriptbox bash ./job.sh arg1 arg2`.

### Switches

Behaviour toggles are settable two ways with the same names (a CLI flag or a
`# /// scriptbox` key), and a flag beats frontmatter beats the default:

| Switch | Flag | Frontmatter | Modes (default first) |
|--------|------|-------------|------------------------|
| `$0` handling | `--argv0 <mode>` | `argv0 = "<mode>"` | `rewrite`, `source`, `off` |
| child protection | `--subscripts` | `subscripts = "freeze"` | `off`, `report`, `freeze` (bare flag = `freeze`) |

```sh
#!/usr/bin/env scriptbox
# /// scriptbox
# interpreter = "dash"
# argv0 = "source"       # real $0 on dash/ksh too (see Internal details)
# checksum = "sha256:1f0c..."
# ///
```

### Pinning

`scriptbox pin` computes the checksum *excluding the entire frontmatter block*,
so pasting it back doesn't invalidate it, and flipping a switch later doesn't
either. Only the shebang and script body are pinned.

```console
$ scriptbox pin job.sh
# checksum = "sha256:1f0c..."
```

Drop that into the `# /// scriptbox` block. From then on, any drift in the file's
body makes scriptbox refuse to run it until you re-pin. (An interpreter set only
in frontmatter isn't covered by the pin; put it on the shebang if you need it
pinned too.)

## What this is, and isn't

> **A speed bump, not a security boundary.** Freezing the bytes stops *accidents*
> - a self-editing script, an editor rewrite, a sync client clobbering a
> long-running job. It won't stop someone who can already write your scripts;
> they'll just edit them *before* you run. The `checksum` pin is the part that
> actually resists tampering.

Out of scope: sourced/`exec`'d child scripts (scriptbox freezes only the
top-level script), sandboxing, and Windows. POSIX only: macOS and Linux.

## Internal details

**How the copy is made.** On Linux, a sealed `memfd` (`F_SEAL_WRITE`), genuinely
immutable, no disk. On macOS, a written-then-unlinked private temp file: once
unlinked, no path reaches the bytes, only scriptbox's read-only fd. Both are
seekable regular files (never pipes), so error line numbers stay correct and
re-reading interpreters (e.g. `uv run --script`) can re-open the fd. The
interpreter runs against that fd's path (`/proc/self/fd/N` or `/dev/fd/N`), never
the mutable original.

**Interpreter precedence:** shebang-line argument > frontmatter `interpreter` >
the script's own shebang > `/bin/sh`.

**`$0` and `${BASH_SOURCE[0]}`: the `--argv0` switch.** Because the interpreter
reads from an fd path, `$0`/`${BASH_SOURCE[0]}` would otherwise show that fd path.
`$SCRIPTBOX_SOURCE` (the real path) is always exported; `--argv0` chooses how `$0`
itself is set:

- **`rewrite`** (default) - an in-run reset where the shell supports it. Preserves
  run-mode semantics and line numbers.
- **`source`** - runs the script via `<sh> -c '. <fd> "$@"' <realpath>`, giving the
  real `$0` on *every* POSIX shell (dash/ksh/bash-3.2 included). The trade: it runs
  in sourced mode (top-level `return` becomes legal; the `[[ "${BASH_SOURCE[0]}" ==
  "$0" ]]` sourced-or-executed idiom flips).
- **`off`** - leave `$0` as the fd path.

| Shell | `rewrite` | `source` | how |
|-------|-----------|----------|-----|
| bash >= 5 | real path | real path | `BASH_ARGV0` / dot-source |
| bash 3.2 (macOS `/bin/bash`) | fd path | real path | dot-source only |
| zsh | real path | real path | `0=` / dot-source |
| dash / ksh / sh | fd path | real path | dot-source only |

`${BASH_SOURCE[0]}` (bash) and `${.sh.file}` (ksh) always show the fd path, they
reflect the file actually opened, which is the immutable copy. For self-locating
scripts, read `$SCRIPTBOX_SOURCE`:

```sh
SELF="${SCRIPTBOX_SOURCE:-${BASH_SOURCE[0]}}"
SCRIPT_DIR="$(cd "$(dirname "$SELF")" && pwd)"
```

`SCRIPTBOX_SOURCE` names the script scriptbox launched, but it's an environment
variable, so a child process *inherits* it: an un-wrapped child that reads it
sees its parent's path, not its own. `--subscripts` fixes this for the tree
(each wrapped child re-sets it); for an un-wrapped child, prefer `${BASH_SOURCE[0]}`
there.

**Subscripts (experimental, opt-in).** By default scriptbox freezes only the
top-level script; a `source`d file or a `bash child.sh` reintroduces the hazard
one level down. `--subscripts` extends immutability to a script's children:

- **`freeze`** (the bare flag) - protect the whole tree. Resolvable *shell*
  children (`bash child.sh`, `./x.sh`) are routed through scriptbox so each is
  frozen too (recursively), and resolvable `source`/`.` includes are frozen into
  an inherited immutable fd (`source /dev/fd/N`), so a streaming source (zsh's
  streams) can't change out from under the caller either. The tree runs from one
  launch-scoped, read-only, pin-on-copy snapshot cache, so a script edited
  *between* invocations in the run can't leak in. A depth counter caps runaway
  recursion. Clear stale caches with `scriptbox gc`.
- **`report`** - just detect and list the child sites; change nothing.

```sh
scriptbox --subscripts bash ./deploy.sh        # protect the tree (prebuilt binaries include the analyzer)
scriptbox --subscripts=report bash ./deploy.sh # just look
cargo install --features subscripts --git https://github.com/jhheider/scriptbox  # from source (default is lean)
```

**What it actually covers**, honestly: only *literal* paths freeze. The common
`source "$DIR/lib.sh"` and anything inside a `$(...)` are reported but left as
live reads: the same wall shellcheck hits (SC1090); the eventual answer is a
directive or a runtime trace. So `freeze` closes the easy majority and is honest
about the rest (each site's status is reported). Already-immune interpreters
(python/ruby/node) are left alone. `scriptbox gc` force-clears any caches.

## License

MIT OR Apache-2.0.
