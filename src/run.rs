//! The run path: read -> verify -> pin bytes into an immutable fd -> exec the
//! real interpreter against that fd (never the mutable original path).

use anyhow::{Context, Result, bail};
use std::convert::Infallible;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::config::{Argv0, Subscripts};
use crate::{checksum, frontmatter, interpreter, loader, shebang, subscripts};

/// A fully-parsed request to run a script.
pub struct RunSpec {
    /// Tokens before the script path: an interpreter and any of its flags
    /// (from a `-S scriptbox bash -x` shebang, or an explicit invocation).
    /// Empty when the interpreter must be resolved from frontmatter/shebang.
    pub interp_override: Vec<String>,
    pub script: PathBuf,
    pub script_args: Vec<String>,
    /// `$0` handling; `None` = defer to frontmatter, then the default.
    pub argv0: Option<Argv0>,
    /// Subscript analysis; `None` = defer to frontmatter, then the default.
    pub subscripts: Option<Subscripts>,
}

/// Everything needed to launch the interpreter, computed without any
/// side effects on the process (no `exec`). Splitting this out from [`run`]
/// keeps the decision logic - checksum gate, interpreter resolution, `$0`
/// rewrite, immutable-copy creation - testable in-process, since `run` itself
/// ends in `exec` and can never return on success.
struct Plan {
    interp: String,
    /// `[interp_args..., fd_path, script_args...]`
    argv: Vec<String>,
    /// Exported as `$SCRIPTBOX_SOURCE`.
    source: String,
    /// The immutable copy, kept alive (and thus its fd open) until `exec`.
    immutable: loader::ImmutableScript,
}

/// Read, verify, resolve, freeze - everything up to but not including `exec`.
fn plan(spec: &RunSpec) -> Result<Plan> {
    let real_path = std::fs::canonicalize(&spec.script).unwrap_or_else(|_| spec.script.clone());
    let source = real_path.to_string_lossy().into_owned();

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

    // Resolve the switches: CLI flag > frontmatter > default.
    let argv0 = resolve_argv0(spec, &fm)?;
    let subs = resolve_subscripts(spec, &fm)?;

    // Subscript analysis (opt-in; errors if requested but built without the
    // `subscripts` feature). Report-only for now: it doesn't freeze children.
    if subs == Subscripts::Report {
        subscripts::report(&bytes, &spec.script)?;
    }

    let (interp, interp_args) = resolve_interpreter(spec, &fm, &bytes);

    // Only `Rewrite` mode alters the served bytes; `Source`/`Off` serve verbatim.
    let served = interpreter::prepare_bytes(&bytes, &interp, &source, argv0 == Argv0::Rewrite);
    let immutable = loader::immutable(&served)?;

    let mut argv = interp_args;
    match argv0 {
        Argv0::Source => {
            // interp [iflags] -c '. <fd> "$@"' <realpath> [script_args...]
            // `<realpath>` becomes $0; `"$@"` expands to the script's args.
            argv.push("-c".to_string());
            argv.push(format!(
                ". {} \"$@\"",
                interpreter::shell_squote(&immutable.fd_path)
            ));
            argv.push(source.clone());
            argv.extend(spec.script_args.iter().cloned());
        }
        Argv0::Rewrite | Argv0::Off => {
            // interp [iflags] <fd_path> [script_args...]
            argv.push(immutable.fd_path.clone());
            argv.extend(spec.script_args.iter().cloned());
        }
    }

    Ok(Plan {
        interp,
        argv,
        source,
        immutable,
    })
}

fn resolve_argv0(spec: &RunSpec, fm: &frontmatter::Frontmatter) -> Result<Argv0> {
    if let Some(m) = spec.argv0 {
        return Ok(m);
    }
    match &fm.argv0 {
        Some(s) => Argv0::parse(s).context("frontmatter `argv0`"),
        None => Ok(Argv0::DEFAULT),
    }
}

fn resolve_subscripts(spec: &RunSpec, fm: &frontmatter::Frontmatter) -> Result<Subscripts> {
    if let Some(m) = spec.subscripts {
        return Ok(m);
    }
    match &fm.subscripts {
        Some(s) => Subscripts::parse(s).context("frontmatter `subscripts`"),
        None => Ok(Subscripts::DEFAULT),
    }
}

/// Execute the script. On success this never returns (the process image is
/// replaced); it only returns `Err` if something fails before/at `exec`.
pub fn run(spec: RunSpec) -> Result<Infallible> {
    let plan = plan(&spec)?;
    // Keep the immutable copy's fd open across exec.
    let _keep = &plan.immutable;

    let mut cmd = Command::new(&plan.interp);
    cmd.args(&plan.argv)
        // Universal escape hatch for self-locating scripts: the real path is
        // always here even though `$0`/`BASH_SOURCE` may show the fd path.
        .env("SCRIPTBOX_SOURCE", &plan.source);

    // Replace this process with the interpreter. Returns only on failure.
    let err = cmd.exec();
    Err(anyhow::Error::new(err).context(format!("exec interpreter `{}`", plan.interp)))
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
                argv0: None,
                subscripts: None,
            },
            frontmatter::Frontmatter {
                interpreter: bytes_interp.map(String::from),
                ..Default::default()
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

    // --- plan(): exercises the full read/verify/resolve/freeze path in-process,
    // without exec (which is what makes the run path invisible to coverage). ---

    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(contents: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "scriptbox-plan.{}.{}.sh",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn run_spec(script: PathBuf, interp: &[&str], argv0: Argv0) -> RunSpec {
        RunSpec {
            interp_override: interp.iter().map(|s| s.to_string()).collect(),
            script,
            script_args: vec!["A".into(), "B".into()],
            argv0: Some(argv0),
            subscripts: None,
        }
    }

    #[test]
    fn plan_builds_argv_and_freezes_the_bytes() {
        let path = tmp("#!/bin/bash\necho hi\n");
        let p = plan(&run_spec(path.clone(), &["bash"], Argv0::Off)).unwrap();
        assert_eq!(p.interp, "bash");
        // argv = [fd_path, A, B]  (no interp flags here)
        assert_eq!(p.argv.len(), 3);
        assert_eq!(p.argv[1], "A");
        assert_eq!(p.argv[2], "B");
        assert!(p.argv[0].starts_with("/dev/fd/") || p.argv[0].starts_with("/proc/self/fd/"));
        // The fd serves exactly the original bytes (no rewrite requested).
        let served = std::fs::read(&p.immutable.fd_path).unwrap();
        assert_eq!(served, b"#!/bin/bash\necho hi\n");
        // SCRIPTBOX_SOURCE is the canonical real path.
        assert_eq!(
            p.source,
            std::fs::canonicalize(&path).unwrap().to_string_lossy()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_applies_argv0_rewrite_to_the_served_copy() {
        let path = tmp("#!/usr/bin/env -S scriptbox bash\necho hi\n");
        let p = plan(&run_spec(path.clone(), &["bash"], Argv0::Rewrite)).unwrap();
        let served = String::from_utf8(std::fs::read(&p.immutable.fd_path).unwrap()).unwrap();
        // Line 1 swapped for the BASH_ARGV0 reset; line 2 preserved.
        assert!(served.starts_with("BASH_ARGV0="), "got: {served:?}");
        assert!(served.ends_with("\necho hi\n"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_refuses_on_checksum_mismatch() {
        let path =
            tmp("#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:deadbeef\"\n# ///\necho hi\n");
        let err = plan(&run_spec(path.clone(), &["bash"], Argv0::Off))
            .err()
            .expect("checksum mismatch should be an error");
        assert!(format!("{err:#}").contains("checksum mismatch"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_runs_when_the_pin_matches() {
        // Pin the bytes (excluding the checksum line), write it in, then plan.
        let template =
            "#!/bin/bash\n# /// scriptbox\n# checksum = \"PLACEHOLDER\"\n# ///\necho hi\n";
        let pin = checksum::pin_of(template.as_bytes());
        let path = tmp(&template.replace("PLACEHOLDER", &pin));
        assert!(plan(&run_spec(path.clone(), &["bash"], Argv0::Off)).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_resolves_interpreter_from_frontmatter_without_argv() {
        let path = tmp(
            "#!/usr/bin/env scriptbox\n# /// scriptbox\n# interpreter = \"zsh\"\n# ///\necho hi\n",
        );
        let p = plan(&run_spec(path.clone(), &[], Argv0::Off)).unwrap();
        assert_eq!(p.interp, "zsh");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_source_mode_builds_a_dot_source_invocation() {
        let path = tmp("#!/usr/bin/env -S scriptbox dash\necho hi\n");
        let p = plan(&run_spec(path.clone(), &["dash"], Argv0::Source)).unwrap();
        // argv = [-c, ". <fd> \"$@\"", <realpath>, A, B]
        assert_eq!(p.argv[0], "-c");
        assert!(p.argv[1].starts_with(". ") && p.argv[1].ends_with("\"$@\""));
        assert!(p.argv[1].contains("/dev/fd/") || p.argv[1].contains("/proc/self/fd/"));
        assert_eq!(p.argv[2], p.source); // $0 = real path
        assert_eq!(&p.argv[3..], &["A", "B"]);
        // Source mode serves the bytes verbatim (no $0 rewrite in the buffer).
        assert_eq!(
            std::fs::read(p.argv[1].split(' ').nth(1).unwrap().trim_matches('\'')).unwrap(),
            b"#!/usr/bin/env -S scriptbox dash\necho hi\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn run_returns_err_when_the_interpreter_is_missing() {
        // A guaranteed-missing interpreter: exec fails, so run() returns Err
        // instead of replacing this test process. Covers the exec-failure tail.
        let path = tmp("#!/bin/sh\ntrue\n");
        let spec = RunSpec {
            interp_override: vec!["/nonexistent/scriptbox-interp-xyz".into()],
            script: path.clone(),
            script_args: vec![],
            argv0: Some(Argv0::Off),
            subscripts: None,
        };
        assert!(run(spec).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
