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

`bash`, `zsh`, `dash`, and `ksh` all do this - POSIX practically requires it.
Everything is fine until the one time it isn't.

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
cargo install --git https://github.com/jhheider/scriptbox
```

`brew install jhheider/tap/scriptbox` and `cargo install scriptbox` land with the
first public release.

## What it does

1. **Freezes the script.** It reads the whole file once into a copy nothing can
   reach to change, then runs the interpreter against *that* - so a mid-run edit
   to the original can't rewrite what's already executing.
2. **Pins it, if you want.** Add a `sha256` checksum and scriptbox refuses to run
   on any drift. `scriptbox pin ./job.sh` prints the line to paste; `scriptbox
   hash ./job.sh` prints just the digest.
3. **Keeps your script locatable.** `$SCRIPTBOX_SOURCE` always points at the real
   file, and `$0` is rewritten back to it where the shell allows.

## Usage

Two ways to name the interpreter - on the shebang line, or in a PEP-723-style
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

### Pinning

`scriptbox pin` computes the checksum *excluding the checksum line itself*, so
pasting it back doesn't invalidate it - no fixpoint to chase:

```console
$ scriptbox pin job.sh
# checksum = "sha256:1f0c..."
```

Drop that into the `# /// scriptbox` block. From then on, any drift in the file's
other bytes makes scriptbox refuse to run it until you re-pin.

## What this is, and isn't

> **A speed bump, not a security boundary.** Freezing the bytes stops *accidents*
> - a self-editing script, an editor rewrite, a sync client clobbering a
> long-running job. It won't stop someone who can already write your scripts;
> they'll just edit them *before* you run. The `checksum` pin is the part that
> actually resists tampering.

Out of scope: sourced/`exec`'d child scripts (scriptbox freezes only the
top-level script), sandboxing, and Windows. POSIX only: macOS and Linux.

## Internal details

**How the copy is made.** On Linux, a sealed `memfd` (`F_SEAL_WRITE`) - genuinely
immutable, no disk. On macOS, a written-then-unlinked private temp file: once
unlinked, no path reaches the bytes, only scriptbox's read-only fd. Both are
seekable regular files (never pipes), so error line numbers stay correct and
re-reading interpreters (e.g. `uv run --script`) can re-open the fd. The
interpreter runs against that fd's path (`/proc/self/fd/N` or `/dev/fd/N`), never
the mutable original.

**Interpreter precedence:** shebang-line argument > frontmatter `interpreter` >
the script's own shebang > `/bin/sh`.

**`$0` and `${BASH_SOURCE[0]}`.** Because the interpreter reads from an fd path,
`$0` and `${BASH_SOURCE[0]}` would otherwise show that fd path. scriptbox always
exports `$SCRIPTBOX_SOURCE` with the real path, and rewrites `$0` back to it where
the shell supports an in-run reset (default; disable with `--no-argv0-rewrite`):

| Shell | `$0` after rewrite | how |
|-------|--------------------|-----|
| bash >= 5 | real path | `BASH_ARGV0` |
| bash 3.2 (macOS `/bin/bash`) | fd path | no mechanism; use `$SCRIPTBOX_SOURCE` |
| zsh | real path | `0=` |
| dash / ksh / sh | fd path | no mechanism; use `$SCRIPTBOX_SOURCE` |

`${BASH_SOURCE[0]}` (bash) and `${.sh.file}` (ksh) always show the fd path - they
reflect the file actually opened, which is the immutable copy. For self-locating
scripts, read `$SCRIPTBOX_SOURCE`:

```sh
SELF="${SCRIPTBOX_SOURCE:-${BASH_SOURCE[0]}}"
SCRIPT_DIR="$(cd "$(dirname "$SELF")" && pwd)"
```

Rewriting `$0` makes the `[[ "${BASH_SOURCE[0]}" == "$0" ]]`
"sourced-or-executed?" idiom see them differ; pass `--no-argv0-rewrite` if a
script relies on it.

## License

MIT OR Apache-2.0.
