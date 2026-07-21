//! Launch-scoped snapshot cache for `--subscripts=freeze`.
//!
//! The first scriptbox in a tree creates a private cache directory (mode 0700)
//! and exports its path as `$SCRIPTBOX_CACHE`, which every descendant inherits.
//! A script is frozen into the cache the first time *any* invocation in the tree
//! encounters it: copied in, pinned (its exact-bytes sha256 recorded), and made
//! read-only (mode 0400). Every later invocation of the same canonical path
//! reuses that snapshot after re-verifying its pin, so the whole tree runs
//! against one consistent, tamper-checked set of bytes even if a script is
//! edited on disk mid-run.

use anyhow::{Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::checksum;

pub const ENV_VAR: &str = "SCRIPTBOX_CACHE";
const PREFIX: &str = "scriptbox-cache.";

fn base_dir() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Remove all `freeze` snapshot cache directories from `$TMPDIR`. Since the root
/// `exec`s away, a tree can't clean up after itself, and reaping automatically
/// on a later launch is unsafe (a detached background job can outlive the root
/// pid, so "root gone" doesn't mean "tree done"). So cleanup is this explicit
/// command, run when no scriptbox trees are active (`$TMPDIR` is also reaped by
/// the OS on reboot). The caches are small.
pub fn gc() -> Result<()> {
    let base = base_dir();
    let mut removed = 0usize;
    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no cache directories under {}", base.display());
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", base.display())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(PREFIX) || !entry.path().is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                removed += 1;
                println!("removed {}", entry.path().display());
            }
            Err(e) => eprintln!(
                "scriptbox: gc: could not remove {}: {e}",
                entry.path().display()
            ),
        }
    }
    println!(
        "removed {removed} cache director{}",
        if removed == 1 { "y" } else { "ies" }
    );
    Ok(())
}

/// Get the cache directory from the environment, or create a fresh one. The
/// returned path should be exported as `$SCRIPTBOX_CACHE` on the interpreter so
/// descendants share it.
pub fn get_or_create() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os(ENV_VAR) {
        return Ok(PathBuf::from(d));
    }
    let base = base_dir();
    let pid = std::process::id();
    for n in 0..128u32 {
        let dir = base.join(format!("{PREFIX}{pid}.{n}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("locking down cache dir `{}`", dir.display()))?;
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("creating snapshot cache dir"),
        }
    }
    anyhow::bail!(
        "could not create a snapshot cache dir under {}",
        base.display()
    )
}

/// Return the frozen bytes for `canonical` (a canonicalized script path), keyed
/// by path. On a cache hit the stored snapshot is returned after its pin is
/// re-verified; on a miss `disk_bytes` are copied in (pinned, mode 0400) and
/// returned.
pub fn frozen_bytes(cache_dir: &Path, canonical: &Path, disk_bytes: &[u8]) -> Result<Vec<u8>> {
    let key = checksum::sha256_pin(canonical.to_string_lossy().as_bytes());
    let key = key.strip_prefix("sha256:").unwrap_or(&key);
    let snap = cache_dir.join(format!("{key}.snap"));
    let pin_file = cache_dir.join(format!("{key}.pin"));

    if snap.exists() {
        let bytes = std::fs::read(&snap)
            .with_context(|| format!("reading cached snapshot `{}`", snap.display()))?;
        let want = std::fs::read_to_string(&pin_file).unwrap_or_default();
        let got = checksum::sha256_pin(&bytes);
        if want.trim() != got {
            anyhow::bail!(
                "cached snapshot for `{}` failed its pin; the cache was modified.\n  \
                 expected: {}\n  actual:   {}",
                canonical.display(),
                want.trim(),
                got
            );
        }
        return Ok(bytes);
    }

    // Miss: pin on copy. Write each file to a private temp then atomically
    // rename into place, so a concurrent freezer (freeze-tree's whole point is a
    // shared, multi-process cache; parallel `a & b & wait` branches hit this)
    // never sees a half-written file or races a 0400-locked destination. The
    // snapshot is renamed LAST, so anyone who sees `.snap` also sees `.pin`.
    let pin = checksum::sha256_pin(disk_bytes);
    write_atomic(&pin_file, pin.as_bytes())?;
    write_atomic(&snap, disk_bytes)?;
    Ok(disk_bytes.to_vec())
}

/// Write `bytes` to `dest` atomically and read-only (0400): to a unique temp in
/// the same dir, then `rename` over `dest`. Concurrent writers each rename their
/// own temp; the last wins and the content is identical, so no reader ever sees
/// a partial or permission-locked file.
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = dest.parent().unwrap_or(Path::new("."));
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp.{}.{n}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing temp `{}`", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o400))
        .with_context(|| format!("locking temp `{}` read-only", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming into `{}`", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "scriptbox-cachetest.{}.{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn first_encounter_caches_then_reuses_ignoring_disk_edits() {
        let dir = scratch();
        let canonical = dir.join("a.sh");

        // Miss: returns disk bytes, caches them read-only.
        let out1 = frozen_bytes(&dir, &canonical, b"original\n").unwrap();
        assert_eq!(out1, b"original\n");
        let key = checksum::sha256_pin(canonical.to_string_lossy().as_bytes());
        let snap = dir.join(format!("{}.snap", key.strip_prefix("sha256:").unwrap()));
        assert_eq!(
            std::fs::metadata(&snap).unwrap().permissions().mode() & 0o777,
            0o400
        );

        // Hit: a *different* on-disk view is ignored; the snapshot wins.
        let out2 = frozen_bytes(&dir, &canonical, b"EDITED-later\n").unwrap();
        assert_eq!(
            out2, b"original\n",
            "the cached snapshot must win over disk edits"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampering_the_cache_is_detected() {
        let dir = scratch();
        let canonical = dir.join("b.sh");
        frozen_bytes(&dir, &canonical, b"trusted\n").unwrap();

        let key = checksum::sha256_pin(canonical.to_string_lossy().as_bytes());
        let snap = dir.join(format!("{}.snap", key.strip_prefix("sha256:").unwrap()));
        std::fs::set_permissions(&snap, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&snap, b"tampered\n").unwrap();

        assert!(frozen_bytes(&dir, &canonical, b"trusted\n").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
