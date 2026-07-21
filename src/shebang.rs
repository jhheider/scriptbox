//! Interpreter detection from a script's own `#!` first line.
//!
//! Used only as a fallback when neither an explicit argv interpreter (from a
//! `-S scriptbox <interp>` shebang) nor a frontmatter `interpreter` is present.
//! Parsed by scriptbox itself rather than delegated to the OS, because macOS and
//! Linux split shebang arguments differently and `env -S` semantics diverge.

/// A parsed shebang: the resolved interpreter and any leading arguments it
/// carries (e.g. the `-x` in `#!/bin/bash -x`).
#[derive(Debug, PartialEq, Eq)]
pub struct Shebang {
    pub interpreter: String,
    pub args: Vec<String>,
}

/// Extract the interpreter from a `#!` first line, resolving `/usr/bin/env`
/// (with or without a leading `-S`). Returns `None` when the first line is not
/// a shebang, or when it points back at scriptbox itself (which must not
/// recurse; that case is handled by the argv interpreter override instead).
pub fn parse(bytes: &[u8]) -> Option<Shebang> {
    let line_end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let line = std::str::from_utf8(&bytes[..line_end]).ok()?;
    // Tolerate a leading UTF-8 BOM and trailing CR.
    let line = line.trim_start_matches('\u{feff}').trim_end_matches('\r');
    let rest = line.strip_prefix("#!")?.trim();

    let mut toks: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    if toks.is_empty() {
        return None;
    }
    let mut interp = toks.remove(0);

    if basename(&interp) == "env" {
        // Drop env's own options (e.g. `-S`, `-i`), then the real interpreter is
        // the next bare token.
        while toks.first().is_some_and(|t| t.starts_with('-')) {
            toks.remove(0);
        }
        if toks.is_empty() {
            return None;
        }
        interp = toks.remove(0);
    }

    if basename(&interp) == "scriptbox" {
        return None;
    }
    Some(Shebang {
        interpreter: interp,
        args: toks,
    })
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(i: &str, a: &[&str]) -> Option<Shebang> {
        Some(Shebang {
            interpreter: i.into(),
            args: a.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn plain_absolute_interpreter() {
        assert_eq!(parse(b"#!/bin/bash\necho\n"), sb("/bin/bash", &[]));
    }

    #[test]
    fn interpreter_with_flag() {
        assert_eq!(parse(b"#!/bin/bash -x\n"), sb("/bin/bash", &["-x"]));
    }

    #[test]
    fn env_resolves_to_next_token() {
        assert_eq!(parse(b"#!/usr/bin/env bash\n"), sb("bash", &[]));
    }

    #[test]
    fn env_dash_s_drops_options() {
        assert_eq!(parse(b"#!/usr/bin/env -S zsh -f\n"), sb("zsh", &["-f"]));
    }

    #[test]
    fn scriptbox_shebang_does_not_recurse() {
        assert_eq!(parse(b"#!/usr/bin/env -S scriptbox bash\n"), None);
    }

    #[test]
    fn no_shebang() {
        assert_eq!(parse(b"echo hi\n"), None);
    }
}
