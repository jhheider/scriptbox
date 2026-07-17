//! The run path: read → verify → pin bytes into an immutable fd → exec the
//! real interpreter against that fd (never the mutable original path).

use anyhow::{Result, bail};
use std::convert::Infallible;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::{checksum, frontmatter, interpreter, loader, shebang};

/// A fully-parsed request to run a script.
pub struct RunSpec {
    /// Tokens before the script path: an interpreter and any of its flags
    /// (from a `-S scriptbox bash -x` shebang, or an explicit invocation).
    /// Empty when the interpreter must be resolved from frontmatter/shebang.
    pub interp_override: Vec<String>,
    pub script: PathBuf,
    pub script_args: Vec<String>,
    /// Reset `$0` to the real path where the interpreter supports it.
    pub rewrite_argv0: bool,
}

/// Execute the script. On success this never returns (the process image is
/// replaced); it only returns `Err` if something fails before/at `exec`.
pub fn run(spec: RunSpec) -> Result<Infallible> {
    let real_path = std::fs::canonicalize(&spec.script).unwrap_or_else(|_| spec.script.clone());
    let real_str = real_path.to_string_lossy().into_owned();

    let bytes = loader::read_script(&spec.script)?;
    let fm = frontmatter::parse(&bytes);

    // Integrity gate first, over the ORIGINAL bytes (so a pin matches the file
    // on disk, independent of any $0 rewrite we apply below).
    if let Some(expected) = fm.checksum.as_deref() {
        let actual = checksum::pin_of(&bytes);
        if !checksum::pins_match(expected, &actual) {
            bail!(
                "checksum mismatch for `{}`\n  expected: {}\n  actual:   {}\n\
                 the script on disk does not match its pinned checksum; refusing to run.\n\
                 if this change is intended, update the pin with `scriptbox pin {}`.",
                spec.script.display(),
                expected.trim(),
                actual,
                spec.script.display(),
            );
        }
    }

    let (interp, interp_args) = resolve_interpreter(&spec, &fm, &bytes);

    let served = interpreter::prepare_bytes(&bytes, &interp, &real_str, spec.rewrite_argv0);
    let immutable = loader::immutable(&served)?;

    // interp [interp_args…] <fd_path> [script_args…]
    let mut cmd = Command::new(&interp);
    cmd.args(&interp_args)
        .arg(&immutable.fd_path)
        .args(&spec.script_args)
        // Universal escape hatch for self-locating scripts: the real path is
        // always here even though `$0`/`BASH_SOURCE` may show the fd path.
        .env("SCRIPTBOX_SOURCE", &real_str);

    // Replace this process with the interpreter. Returns only on failure.
    let err = cmd.exec();
    Err(anyhow::Error::new(err).context(format!("exec interpreter `{interp}`")))
}

/// Interpreter precedence: explicit argv override > frontmatter > the script's
/// own shebang > `/bin/sh`.
fn resolve_interpreter(
    spec: &RunSpec,
    fm: &frontmatter::Frontmatter,
    bytes: &[u8],
) -> (String, Vec<String>) {
    if let Some((first, rest)) = spec.interp_override.split_first() {
        return (first.clone(), rest.to_vec());
    }
    if let Some(i) = fm.interpreter.clone() {
        return (i, Vec::new());
    }
    if let Some(sb) = shebang::parse(bytes) {
        return (sb.interpreter, sb.args);
    }
    ("/bin/sh".to_string(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        interp_override: &[&str],
        bytes_interp: Option<&str>,
    ) -> (RunSpec, frontmatter::Frontmatter) {
        (
            RunSpec {
                interp_override: interp_override.iter().map(|s| s.to_string()).collect(),
                script: PathBuf::from("x.sh"),
                script_args: vec![],
                rewrite_argv0: true,
            },
            frontmatter::Frontmatter {
                interpreter: bytes_interp.map(String::from),
                checksum: None,
            },
        )
    }

    #[test]
    fn argv_override_wins_over_frontmatter_and_shebang() {
        let (s, fm) = spec(&["bash", "-x"], Some("zsh"));
        let (i, a) = resolve_interpreter(&s, &fm, b"#!/bin/dash\n");
        assert_eq!(i, "bash");
        assert_eq!(a, vec!["-x"]);
    }

    #[test]
    fn frontmatter_wins_over_shebang() {
        let (s, fm) = spec(&[], Some("zsh"));
        let (i, _) = resolve_interpreter(&s, &fm, b"#!/bin/dash\n");
        assert_eq!(i, "zsh");
    }

    #[test]
    fn falls_back_to_script_shebang_then_sh() {
        let (s, fm) = spec(&[], None);
        assert_eq!(resolve_interpreter(&s, &fm, b"#!/bin/ksh\n").0, "/bin/ksh");
        assert_eq!(resolve_interpreter(&s, &fm, b"echo hi\n").0, "/bin/sh");
    }
}
