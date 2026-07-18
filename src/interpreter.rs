//! Per-interpreter `$0` handling.
//!
//! Because the interpreter reads the script from an fd path (`/dev/fd/N` or
//! `/proc/self/fd/N`), `$0` and `${BASH_SOURCE[0]}` see that fd path rather than
//! the real script path. `SCRIPTBOX_SOURCE` (exported unconditionally by the
//! runner) is the universal escape hatch, but for the common `usage: $0` /
//! `dirname "$0"` case we can also reset `$0` in-run - where the shell supports
//! it - by prepending a `$0` reset to the first body line, joined with `;`.
//!
//! Prepending onto the first body line (rather than swapping the shebang, or
//! inserting a new line) buys two things at once: no line is added, so every
//! original line number is preserved exactly, and the shebang (if any) stays on
//! line 1, so the served copy stays lint-clean (same shell dialect, no
//! "missing shebang"). The mashed-together first line is never seen by a human -
//! it's the internal frozen copy - so that's a zero-cost trick.
//!
//! What each shell supports for an *in-run* `$0` reset (probed empirically):
//! - **bash >= 5**: `BASH_ARGV0='...'` (on bash 3.2 it's a harmless plain var).
//! - **zsh**: `0='...'` (direct assignment).
//! - **dash / ksh / sh / other**: no in-run mechanism - `SCRIPTBOX_SOURCE` only.
//!
//! Trade-off when we do rewrite `$0`: `${BASH_SOURCE[0]}` still shows the fd
//! path, so the `[[ "${BASH_SOURCE[0]}" == "$0" ]]` "sourced-or-executed?" idiom
//! sees them differ. That idiom is the documented casualty of `--argv0-rewrite`.

/// The mechanism (if any) for resetting `$0` in-run for an interpreter.
#[derive(Debug, PartialEq, Eq)]
enum Argv0Fix {
    /// `BASH_ARGV0='path'`
    BashArgv0,
    /// `0='path'`
    ZshZero,
    /// No in-run mechanism.
    None,
}

fn argv0_fix(interp: &str) -> Argv0Fix {
    match basename(interp) {
        "bash" => Argv0Fix::BashArgv0,
        "zsh" => Argv0Fix::ZshZero,
        _ => Argv0Fix::None,
    }
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Single-quote a string for safe inclusion in a POSIX shell word.
pub fn shell_squote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''"); // close, escaped-quote, reopen
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Produce the bytes to hand the interpreter, applying the `Rewrite` `$0`
/// mechanism when asked (and the interpreter supports an in-run `$0` reset).
///
/// The `$0` reset is **prepended to the first body line, joined with `;`** - after
/// the shebang line if there is one, else at the very start. This adds no line, so
/// every original line number is preserved exactly (error messages stay accurate),
/// and the shebang stays on line 1, so the served copy is lint-clean - scriptbox
/// adds no findings a linter wouldn't already report on the original.
///
/// The one exception is a shebang with no body (nothing after it): there's no line
/// to join onto, so the reset is appended on its own line (harmless - no body to
/// misnumber). In every non-rewrite case the original bytes are returned verbatim
/// (`Source`/`Off` serve verbatim; `Source` gets `$0` from the dot-source call).
///
/// Independent of the checksum gate, which runs over the pre-rewrite bytes, so a
/// pin verifies the file on disk whether or not a shebang is present.
pub fn prepare_bytes(original: &[u8], interp: &str, real_path: &str, rewrite: bool) -> Vec<u8> {
    if !rewrite {
        return original.to_vec();
    }
    let prologue = match argv0_fix(interp) {
        Argv0Fix::BashArgv0 => format!("BASH_ARGV0={}", shell_squote(real_path)),
        Argv0Fix::ZshZero => format!("0={}", shell_squote(real_path)),
        Argv0Fix::None => return original.to_vec(),
    };

    // Where the body starts: after the shebang's newline, else the very start.
    let split = if original.starts_with(b"#!") {
        match original.iter().position(|&b| b == b'\n') {
            Some(nl) => nl + 1,
            // A shebang with no newline at all: no body to join onto. Append the
            // reset on its own line (there are no body lines to misnumber).
            None => {
                let mut out = original.to_vec();
                out.push(b'\n');
                out.extend_from_slice(prologue.as_bytes());
                out.push(b'\n');
                return out;
            }
        }
    } else {
        0
    };

    // Prepend the reset to the first body line with `; ` - no new line.
    let mut out = original[..split].to_vec();
    out.extend_from_slice(prologue.as_bytes());
    out.extend_from_slice(b"; ");
    out.extend_from_slice(&original[split..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rewrite_returns_verbatim() {
        let src = b"#!/bin/bash\necho hi\n";
        assert_eq!(prepare_bytes(src, "bash", "/real.sh", false), src);
    }

    #[test]
    fn bash_rewrite_joins_onto_the_first_body_line() {
        let src = b"#!/usr/bin/env -S scriptbox bash\necho hi\nfalse\n";
        let out = prepare_bytes(src, "bash", "/home/j/deploy.sh", true);
        // Shebang on line 1 (lint-clean); reset joined onto the first body line
        // with `;`, so `echo hi` and `false` keep their original line numbers.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#!/usr/bin/env -S scriptbox bash\nBASH_ARGV0='/home/j/deploy.sh'; echo hi\nfalse\n"
        );
    }

    #[test]
    fn zsh_uses_bare_zero_assignment() {
        let out = prepare_bytes(b"#!/bin/zsh\necho\n", "/bin/zsh", "/a b.sh", true);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#!/bin/zsh\n0='/a b.sh'; echo\n"
        );
    }

    #[test]
    fn dash_has_no_in_run_fix_so_bytes_are_verbatim() {
        let src = b"#!/bin/dash\necho\n";
        assert_eq!(prepare_bytes(src, "dash", "/real.sh", true), src);
    }

    #[test]
    fn no_shebang_joins_onto_line_one() {
        // No shebang line, so the reset joins onto the original line 1 - no line
        // added, line numbers preserved, code never deleted.
        let out = prepare_bytes(b"echo first\nfalse\n", "bash", "/real.sh", true);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "BASH_ARGV0='/real.sh'; echo first\nfalse\n"
        );
    }

    #[test]
    fn no_shebang_zsh_joins_onto_line_one() {
        let out = prepare_bytes(b"print $0\n", "zsh", "/a b.sh", true);
        assert_eq!(String::from_utf8(out).unwrap(), "0='/a b.sh'; print $0\n");
    }

    #[test]
    fn embedded_single_quote_is_escaped() {
        let out = prepare_bytes(b"#!/bin/bash\n:\n", "bash", "/o'brien/x.sh", true);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#!/bin/bash\nBASH_ARGV0='/o'\\''brien/x.sh'; :\n"
        );
    }

    #[test]
    fn shebang_with_no_body_appends_the_reset() {
        let out = prepare_bytes(b"#!/bin/bash", "bash", "/x.sh", true);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "#!/bin/bash\nBASH_ARGV0='/x.sh'\n"
        );
    }
}
