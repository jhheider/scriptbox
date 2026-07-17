//! End-to-end behavioural proof.
//!
//! These tests exercise the built `scriptbox` binary against real shells. The
//! headline tests are *differential*: they run a self-mutating script BOTH under
//! a plain shell and under scriptbox, and assert the plain shell is corrupted
//! while scriptbox is insulated - proving scriptbox changes behaviour, not just
//! that it runs.
//!
//! Shell-specific tests skip (rather than fail) when a shell isn't installed, so
//! the suite is honest on any host while covering whatever is available.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

fn scriptbox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_scriptbox"))
}

fn have(shell: &str) -> bool {
    Command::new(shell)
        .arg("-c")
        .arg("exit 0")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Write `contents` to a uniquely-named file in the temp dir and return its path.
fn write_script(tag: &str, contents: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("scriptbox-e2e.{}.{tag}.{n}.sh", std::process::id()));
    std::fs::write(&path, contents).expect("write temp script");
    path
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A script that appends a "vulnerable" marker to its own file partway through,
/// then finishes. Under a streaming shell the marker executes; under scriptbox
/// it must not. It targets `$SCRIPTBOX_SOURCE` if set (scriptbox), else `$0`
/// (plain shell) - so the same script reproduces the hazard both ways.
fn self_mutating(shell: &str) -> String {
    format!(
        "#!/usr/bin/env -S scriptbox {shell}\n\
         echo START\n\
         printf 'echo INJECTED_MARKER\\n' >> \"${{SCRIPTBOX_SOURCE:-$0}}\"\n\
         echo END\n"
    )
}

const SHELLS: &[&str] = &["bash", "zsh", "dash", "ksh"];

#[test]
fn differential_insulation_across_shells() {
    let mut exercised = 0;
    for &shell in SHELLS {
        if !have(shell) {
            eprintln!("skip: {shell} not installed");
            continue;
        }
        exercised += 1;

        // 1) CONTROL: the plain shell streaming its own file IS corrupted.
        let victim = write_script(&format!("plain-{shell}"), &self_mutating(shell));
        let plain = Command::new(shell)
            .arg(&victim)
            .output()
            .expect("run plain shell");
        assert!(
            stdout(&plain).contains("INJECTED_MARKER"),
            "{shell}: expected the plain shell to be vulnerable (execute the injected \
             line), but it did not. stdout={:?}",
            stdout(&plain)
        );
        let _ = std::fs::remove_file(&victim);

        // 2) scriptbox running the SAME script is insulated: the marker is still
        //    written to disk, but never executed.
        let boxed = write_script(&format!("box-{shell}"), &self_mutating(shell));
        let out = scriptbox()
            .arg(shell)
            .arg(&boxed)
            .output()
            .expect("run scriptbox");
        assert!(
            out.status.success(),
            "{shell}: scriptbox exited nonzero: {}",
            stderr(&out)
        );
        assert!(
            stdout(&out).contains("START") && stdout(&out).contains("END"),
            "{shell}: body did not run: {:?}",
            stdout(&out)
        );
        assert!(
            !stdout(&out).contains("INJECTED_MARKER"),
            "{shell}: scriptbox FAILED to insulate - the injected line executed. stdout={:?}",
            stdout(&out)
        );
        // Prove the mutation really happened on disk (so the test is meaningful).
        let on_disk = std::fs::read_to_string(&boxed).unwrap_or_default();
        assert!(
            on_disk.contains("echo INJECTED_MARKER"),
            "{shell}: the script did not actually mutate itself; test is not exercising the hazard"
        );
        let _ = std::fs::remove_file(&boxed);
    }
    assert!(exercised > 0, "no target shells were available to test");
}

#[test]
fn checksum_matching_runs_and_mismatch_refuses() {
    if !have("bash") {
        return;
    }
    // Build a pinned file. Because the pin excludes the checksum line itself,
    // one `hash` call gives the value that will still match after we paste it.
    let template = "#!/usr/bin/env -S scriptbox bash\n\
         # /// scriptbox\n\
         # checksum = \"PLACEHOLDER\"\n\
         # ///\n\
         echo \"ran with arg=$1\"\n";
    let ppath = write_script("pinned", template);
    let pin = stdout(&scriptbox().arg("hash").arg(&ppath).output().unwrap());
    let pin = pin.trim().to_string();
    assert!(pin.starts_with("sha256:"));
    let pinned = template.replace("PLACEHOLDER", &pin);
    std::fs::write(&ppath, &pinned).unwrap();

    let good = scriptbox()
        .arg("bash")
        .arg(&ppath)
        .arg("hi")
        .output()
        .unwrap();
    assert!(
        good.status.success() && stdout(&good).contains("ran with arg=hi"),
        "matching pin should run: status={:?} stdout={:?} stderr={:?}",
        good.status,
        stdout(&good),
        stderr(&good)
    );

    // Tamper the body -> refuse (the injected line is real content, so the pin
    // no longer matches).
    let mut tampered = pinned.into_bytes();
    tampered.extend_from_slice(b"echo tampered\n");
    std::fs::write(&ppath, &tampered).unwrap();
    let bad = scriptbox().arg("bash").arg(&ppath).output().unwrap();
    assert!(!bad.status.success(), "tampered script must be refused");
    assert!(
        stderr(&bad).contains("checksum mismatch"),
        "expected a checksum-mismatch message, got: {}",
        stderr(&bad)
    );

    let _ = std::fs::remove_file(&ppath);
}

#[test]
fn args_and_exit_code_pass_through() {
    if !have("bash") {
        return;
    }
    let path = write_script(
        "args",
        "#!/usr/bin/env -S scriptbox bash\necho \"args: $*\"\nexit 7\n",
    );
    let out = scriptbox()
        .arg("bash")
        .arg(&path)
        .arg("a")
        .arg("b c")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7), "exit code must propagate");
    assert!(
        stdout(&out).contains("args: a b c"),
        "args must pass through: {:?}",
        stdout(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scriptbox_source_is_always_the_real_path() {
    if !have("bash") {
        return;
    }
    let path = write_script(
        "src",
        "#!/usr/bin/env -S scriptbox bash\necho \"SRC=$SCRIPTBOX_SOURCE\"\n",
    );
    let out = scriptbox().arg("bash").arg(&path).output().unwrap();
    let want = std::fs::canonicalize(&path).unwrap();
    assert!(
        stdout(&out).contains(&format!("SRC={}", want.display())),
        "SCRIPTBOX_SOURCE should be the canonical real path {:?}; got {:?}",
        want,
        stdout(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn interpreter_may_be_given_as_a_path_not_just_a_name() {
    // Regression: an interpreter given as an existing path (/bin/bash) must not
    // be mistaken for the script (it's a program binary, not a text script).
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let path = write_script(
        "interp-path",
        "#!/usr/bin/env -S scriptbox bash\necho OK_PATH_INTERP\n",
    );
    let out = scriptbox().arg("/bin/bash").arg(&path).output().unwrap();
    assert!(
        out.status.success() && stdout(&out).contains("OK_PATH_INTERP"),
        "interpreter-as-path failed: status={:?} stdout={:?} stderr={:?}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn zsh_argv0_rewrite_shows_real_path_and_opt_out_shows_fd() {
    if !have("zsh") {
        return;
    }
    let path = write_script(
        "zsh0",
        "#!/usr/bin/env -S scriptbox zsh\necho \"ZERO=$0\"\n",
    );
    let want = std::fs::canonicalize(&path).unwrap();

    // Default: $0 rewritten to the real path (zsh supports `0=`).
    let on = scriptbox().arg("zsh").arg(&path).output().unwrap();
    assert!(
        stdout(&on).contains(&format!("ZERO={}", want.display())),
        "zsh $0 should be the real path; got {:?}",
        stdout(&on)
    );

    // Opt out: $0 is the fd path.
    let off = scriptbox()
        .arg("--no-argv0-rewrite")
        .arg("zsh")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        stdout(&off).contains("ZERO=/dev/fd/") || stdout(&off).contains("ZERO=/proc/self/fd/"),
        "with --no-argv0-rewrite, zsh $0 should be the fd path; got {:?}",
        stdout(&off)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn error_messages_keep_correct_line_numbers() {
    if !have("bash") {
        return;
    }
    // The undefined command is on line 3; the shebang is line 1.
    let path = write_script(
        "lineno",
        "#!/usr/bin/env -S scriptbox bash\ntrue\nthis_cmd_does_not_exist_zzz\n",
    );
    let out = scriptbox().arg("bash").arg(&path).output().unwrap();
    assert!(
        stderr(&out).contains("line 3"),
        "expected the error to report line 3, got: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn frontmatter_interpreter_is_used_when_no_argv_interpreter() {
    if !have("bash") {
        return;
    }
    // No interpreter on the command line; it comes from the frontmatter.
    let path = write_script(
        "fm-interp",
        "#!/usr/bin/env scriptbox\n# /// scriptbox\n# interpreter = \"bash\"\n# ///\necho \"FM_OK $BASH_VERSION\"\n",
    );
    let out = scriptbox().arg(&path).output().unwrap();
    assert!(
        out.status.success() && stdout(&out).contains("FM_OK"),
        "frontmatter interpreter not honored: status={:?} stdout={:?} stderr={:?}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn argv0_source_mode_gives_dash_a_real_dollar_zero() {
    if !have("dash") {
        return;
    }
    // dash can't set $0 in-run (Rewrite is a no-op); Source mode does it.
    let path = write_script(
        "srcmode",
        "#!/usr/bin/env -S scriptbox dash\necho \"Z=$0\"\n",
    );
    let want = std::fs::canonicalize(&path).unwrap();
    let out = scriptbox()
        .arg("--argv0=source")
        .arg("dash")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains(&format!("Z={}", want.display())),
        "source mode should give dash the real $0; got {:?}",
        stdout(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[cfg(not(feature = "subscripts"))]
#[test]
fn subscripts_flag_errors_without_the_feature() {
    if !have("bash") {
        return;
    }
    let path = write_script(
        "nofeat",
        "#!/usr/bin/env -S scriptbox bash\nsource ./x.sh\n",
    );
    let out = scriptbox()
        .arg("--subscripts")
        .arg("bash")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("without the `subscripts` feature"),
        "expected a build-feature error, got: {}",
        stderr(&out)
    );
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "subscripts")]
#[test]
fn subscripts_reports_resolvable_and_dynamic_sites() {
    if !have("bash") {
        return;
    }
    let path = write_script(
        "subs",
        "#!/usr/bin/env -S scriptbox bash\nsource ./lib.sh\n. \"$HOME/u.sh\"\nbash -c 'true'\necho hi\n",
    );
    let out = scriptbox()
        .arg("--subscripts")
        .arg("bash")
        .arg(&path)
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        err.contains("./lib.sh") && err.contains("resolvable"),
        "report: {err}"
    );
    assert!(
        err.contains("$HOME/u.sh") && err.contains("dynamic"),
        "report: {err}"
    );
    // `bash -c` is inline: it must NOT be reported as a file child.
    assert!(
        !err.contains("[bash"),
        "bash -c should not be a subscript site: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "subscripts")]
#[test]
fn subscripts_wrap_freezes_a_shell_child() {
    if !have("bash") {
        return;
    }
    // The child appends to its own (hardcoded) path mid-run, then finishes.
    let child = write_script("wrapchild", "");
    let child_body = format!(
        "#!/bin/bash\necho CHILD_START\nprintf 'echo CHILD_INJECTED\\n' >> {}\necho CHILD_END\n",
        child.display()
    );
    let parent = write_script(
        "wrapparent",
        &format!(
            "#!/usr/bin/env -S scriptbox bash\nbash {}\n",
            child.display()
        ),
    );

    // report mode: the child runs plain -> vulnerable (the injected line executes).
    std::fs::write(&child, &child_body).unwrap();
    let rep = scriptbox()
        .arg("--subscripts=report")
        .arg("bash")
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        stdout(&rep).contains("CHILD_INJECTED"),
        "report: the un-wrapped child should be vulnerable; got {:?}",
        stdout(&rep)
    );

    // wrap mode: the child is routed through scriptbox -> frozen (never runs it).
    std::fs::write(&child, &child_body).unwrap();
    let wrapped = scriptbox()
        .arg("--subscripts=wrap")
        .arg("bash")
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        stdout(&wrapped).contains("CHILD_START") && stdout(&wrapped).contains("CHILD_END"),
        "child body should still run: {:?}",
        stdout(&wrapped)
    );
    assert!(
        !stdout(&wrapped).contains("CHILD_INJECTED"),
        "wrap: the child must be frozen (no injected line); got {:?}",
        stdout(&wrapped)
    );

    let _ = std::fs::remove_file(&child);
    let _ = std::fs::remove_file(&parent);
}

#[cfg(feature = "subscripts")]
#[test]
fn freeze_tree_ignores_a_cross_invocation_edit() {
    if !have("bash") {
        return;
    }
    // A(first) -> B edits A.sh on disk -> B calls A(second). Under `wrap` the
    // second A re-reads the edit; under `freeze-tree` it reuses the snapshot.
    let a = write_script("ftA", "");
    let b = write_script("ftB", "");
    let a_body = format!(
        "#!/usr/bin/env -S scriptbox bash\necho A:$1\n[ \"$1\" = first ] && bash {}\ntrue\n",
        b.display()
    );
    let b_body = format!(
        "#!/usr/bin/env -S scriptbox bash\nprintf 'echo A_INJECTED\\n' >> {}\nbash {} second\n",
        a.display(),
        a.display()
    );
    let reset = || {
        std::fs::write(&a, &a_body).unwrap();
        std::fs::write(&b, &b_body).unwrap();
    };

    reset();
    let wrapped = scriptbox()
        .arg("--subscripts=wrap")
        .arg("bash")
        .arg(&a)
        .arg("first")
        .output()
        .unwrap();
    assert!(
        stdout(&wrapped).contains("A_INJECTED"),
        "wrap: the cross-invocation edit should leak; got {:?}",
        stdout(&wrapped)
    );

    reset();
    let frozen = scriptbox()
        .arg("--subscripts=freeze-tree")
        .arg("bash")
        .arg(&a)
        .arg("first")
        .output()
        .unwrap();
    assert!(
        stdout(&frozen).contains("A:first") && stdout(&frozen).contains("A:second"),
        "freeze-tree: both invocations should still run: {:?}",
        stdout(&frozen)
    );
    assert!(
        !stdout(&frozen).contains("A_INJECTED"),
        "freeze-tree: the cached snapshot must win over the disk edit; got {:?}",
        stdout(&frozen)
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}
