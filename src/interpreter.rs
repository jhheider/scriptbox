//! Per-interpreter `$0` handling.
//!
//! Because the interpreter reads the script from an fd path (`/dev/fd/N` or
//! `/proc/self/fd/N`), `$0` and `${BASH_SOURCE[0]}` see that fd path rather than
//! the real script path. `SCRIPTBOX_SOURCE` (exported unconditionally by the
//! runner) is the universal escape hatch, but for the common `usage: $0` /
//! `dirname "$0"` case we can also reset `$0` in-run - where the shell supports
//! it - by replacing the line-1 shebang with a one-line prologue.
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

/// Single-quote a string for safe inclusion in a POSIX shell assignment.
fn shell_squote(s: &str) -> String {
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

/// Produce the bytes to hand the interpreter.
///
/// When `rewrite_argv0` is set, the first line is a `#!` shebang (safe to
/// discard, since it's a comment to the interpreter), and the interpreter
/// supports an in-run `$0` reset, line 1 is replaced **one-for-one** with the
/// reset - preserving every subsequent line number exactly. In every other case
/// the original bytes are returned verbatim.
pub fn prepare_bytes(
    original: &[u8],
    interp: &str,
    real_path: &str,
    rewrite_argv0: bool,
) -> Vec<u8> {
    if !rewrite_argv0 || !original.starts_with(b"#!") {
        return original.to_vec();
    }
    let prologue = match argv0_fix(interp) {
        Argv0Fix::BashArgv0 => format!("BASH_ARGV0={}", shell_squote(real_path)),
        Argv0Fix::ZshZero => format!("0={}", shell_squote(real_path)),
        Argv0Fix::None => return original.to_vec(),
    };
    let mut out = prologue.into_bytes();
    // Keep the first newline and everything after it byte-for-byte, so line
    // numbers past line 1 are unchanged.
    if let Some(nl) = original.iter().position(|&b| b == b'\n') {
        out.extend_from_slice(&original[nl..]);
    }
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
    fn bash_rewrite_swaps_line_one_and_preserves_the_rest() {
        let src = b"#!/usr/bin/env -S scriptbox bash\necho hi\nfalse\n";
        let out = prepare_bytes(src, "bash", "/home/j/deploy.sh", true);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "BASH_ARGV0='/home/j/deploy.sh'\necho hi\nfalse\n");
        // Line count is unchanged (line-number fidelity).
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn zsh_uses_bare_zero_assignment() {
        let out = prepare_bytes(b"#!/bin/zsh\necho\n", "/bin/zsh", "/a b.sh", true);
        assert_eq!(String::from_utf8(out).unwrap(), "0='/a b.sh'\necho\n");
    }

    #[test]
    fn dash_has_no_in_run_fix_so_bytes_are_verbatim() {
        let src = b"#!/bin/dash\necho\n";
        assert_eq!(prepare_bytes(src, "dash", "/real.sh", true), src);
    }

    #[test]
    fn non_shebang_first_line_is_never_rewritten() {
        // Would otherwise delete a real line of code.
        let src = b"echo first\n";
        assert_eq!(prepare_bytes(src, "bash", "/real.sh", true), src);
    }

    #[test]
    fn embedded_single_quote_is_escaped() {
        let out = prepare_bytes(b"#!/bin/bash\n:\n", "bash", "/o'brien/x.sh", true);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "BASH_ARGV0='/o'\\''brien/x.sh'\n:\n"
        );
    }
}
