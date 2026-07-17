# scriptbox

Read a script fully into an immutable copy at invoke, verify an optional
checksum, then hand it to the real interpreter — closing the window where
editing a running script (by you, a background process, or malware) changes what
executes past the current line.

```sh
#!/usr/bin/env -S scriptbox bash
echo "this script cannot be edited out from under itself while it runs"
```

## The problem

`bash`, `zsh`, `dash`, and `ksh` read scripts **incrementally** as they execute —
they don't buffer the whole file first (POSIX actually mandates the input file
pointer sit just after the command being run, so a conforming shell *can't*
pre-buffer). If the file changes mid-run — an in-place editor write, a `>>`
append, an `rsync`/Dropbox clobber, a self-rewriting script, or something
hostile — the shell keeps reading from its byte offset into the *new* bytes, and
executes whatever now lands there. Everything is fine until the one time it
isn't.

Interpreters that parse the whole file first (Python, Ruby, Node) don't have this
hazard — but they still benefit from scriptbox's checksum verification.

## What scriptbox does

1. Reads the whole script once, into an immutable copy: a sealed `memfd`
   (`F_SEAL_WRITE`) on Linux, a written-then-unlinked private temp file on macOS.
   Nothing can reach those bytes to change them.
2. Optionally verifies a `sha256` pin and refuses to run on a mismatch.
3. Execs the real interpreter against the immutable copy's fd path
   (`/proc/self/fd/N` or `/dev/fd/N`), never the mutable original — so a mid-run
   edit to the source file can't change what's already running.

It's a shebang loader, not a shell replacement: `./deploy.sh` still works, with
the same arguments, stdin, exit code, and correct line numbers in errors.

## Usage

Two ways to declare the interpreter:

```sh
#!/usr/bin/env -S scriptbox bash          # interpreter on the shebang line
```

```sh
#!/usr/bin/env scriptbox                  # generic shebang + frontmatter
# /// scriptbox
# interpreter = "bash"
# checksum = "sha256:…"
# ///
```

The `# /// scriptbox` block is PEP-723-style: because every line is a `#`
comment, the file still runs under a plain interpreter when scriptbox isn't
installed. Precedence for the interpreter: shebang-line argument > frontmatter >
the script's own shebang > `/bin/sh`.

Explicit and helper forms:

```sh
scriptbox bash ./deploy.sh arg1 arg2      # run explicitly
scriptbox pin  ./deploy.sh                # print a checksum line to paste
scriptbox hash ./deploy.sh                # print just the sha256 pin
```

### Pinning a script

`scriptbox pin` computes the checksum **excluding the checksum line itself**, so
pasting the line back doesn't invalidate it (no fixpoint chasing):

```sh
$ scriptbox pin deploy.sh
# checksum = "sha256:1f0c…"
```

Add that to the script's `# /// scriptbox` block. From then on, any drift in the
file's other bytes makes scriptbox refuse to run it until you re-pin.

## Two distinct guarantees

scriptbox offers two things; keep them separate.

- **Runtime immutability** — the bytes can't change under the interpreter while
  it runs. This defends against *accidental* corruption (self-editing scripts,
  editor rewrites, a sync client overwriting a long-running job) and is a
  defense-in-depth speed bump, **not** a security boundary: an attacker who can
  already write your scripts can just edit them *before* you run, or edit
  scriptbox, or your `PATH`.
- **Integrity / provenance** — the `checksum` pin proves the file is exactly
  what you expect, catching tampering-before-run, wrong versions, and corrupted
  downloads. This is the guarantee that actually resists substitution.

Out of scope: sourced/`exec`'d child scripts (v1 protects only the top-level
script), general sandboxing, and Windows/PowerShell.

## Self-locating scripts (`$0` / `BASH_SOURCE`)

Because the interpreter reads from an fd path, `$0` and `${BASH_SOURCE[0]}`
otherwise see that fd path instead of the real script location. scriptbox
handles this two ways:

- **`$SCRIPTBOX_SOURCE`** is always exported as the real script path. This is the
  reliable, universal escape hatch. For self-locating scripts, use:

  ```sh
  SELF="${SCRIPTBOX_SOURCE:-${BASH_SOURCE[0]}}"
  SCRIPT_DIR="$(cd "$(dirname "$SELF")" && pwd)"
  ```

- **`$0` is rewritten to the real path** where the shell supports an in-run
  reset (default; disable with `--no-argv0-rewrite`). Support varies by shell:

  | Shell | `$0` after rewrite | Notes |
  |-------|--------------------|-------|
  | bash ≥ 5 | real path | via `BASH_ARGV0` |
  | bash 3.2 (macOS `/bin/bash`) | fd path | no `BASH_ARGV0`; use `$SCRIPTBOX_SOURCE` |
  | zsh | real path | via `0=` |
  | dash / ksh / sh | fd path | no in-run mechanism; use `$SCRIPTBOX_SOURCE` |

  `${BASH_SOURCE[0]}` (bash) and `${.sh.file}` (ksh) always show the fd path —
  they reflect the file actually opened, which is fundamentally the immutable
  copy. That's what `$SCRIPTBOX_SOURCE` is for. Note that rewriting `$0` makes
  the `[[ "${BASH_SOURCE[0]}" == "$0" ]]` "sourced-or-executed?" idiom see them
  differ; pass `--no-argv0-rewrite` if a script relies on it.

## Platform support

POSIX only: macOS and Linux. Linux uses a sealed `memfd`; macOS uses an
open-then-unlink private temp file. Both are seekable regular files (never
pipes), so error line numbers stay correct and re-reading interpreters (e.g.
`uv run --script`) can re-open the fd.

## License

MIT OR Apache-2.0.
