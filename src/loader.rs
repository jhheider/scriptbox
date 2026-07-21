//! Reading the script and pinning its bytes into an immutable, inheritable fd.
//!
//! The fd is a **seekable regular file** (a sealed `memfd` on Linux, a
//! written-then-unlinked temp file on macOS), never a pipe, so the
//! interpreter's block-read+seek path works and error messages keep correct
//! line numbers, and so re-reading interpreters (e.g. `uv run --script`) can
//! re-open it to parse inline metadata.

use anyhow::{Context, Result};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

/// Read the whole script into memory.
///
/// On macOS, refuse to materialize an iCloud/Dropbox "dataless" placeholder -
/// reading one triggers a blocking on-demand download that can hang forever if
/// the provider is offline. Report a clear error instead of stalling.
pub fn read_script(path: &Path) -> Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    dataless_guard(path)?;
    std::fs::read(path).with_context(|| format!("reading script `{}`", path.display()))
}

#[cfg(target_os = "macos")]
fn dataless_guard(path: &Path) -> Result<()> {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    let md = std::fs::metadata(path).with_context(|| format!("stat `{}`", path.display()))?;
    if md.st_flags() & SF_DATALESS != 0 {
        anyhow::bail!(
            "`{}` is a dataless (online-only) file; refusing to trigger a cloud \
             download. Materialize it first (open/download it), then re-run.",
            path.display()
        );
    }
    Ok(())
}

/// An immutable, exec-inheritable fd holding the script bytes, together with the
/// fd path the interpreter should read.
pub struct ImmutableScript {
    // Held open until `exec` replaces the process image; never explicitly used
    // after `fd_path` is computed, but must outlive the exec call.
    _fd: OwnedFd,
    /// `/proc/self/fd/N` on Linux, `/dev/fd/N` on macOS.
    pub fd_path: String,
}

/// Pin `bytes` into an immutable fd and return the path to hand the interpreter.
pub fn immutable(bytes: &[u8]) -> Result<ImmutableScript> {
    let fd = make_fd(bytes)?;
    // The fd must survive `exec` so the interpreter can open the fd path.
    clear_cloexec(&fd)?;
    let n = fd.as_raw_fd();
    let fd_path = if cfg!(target_os = "linux") {
        format!("/proc/self/fd/{n}")
    } else {
        format!("/dev/fd/{n}")
    };
    Ok(ImmutableScript { _fd: fd, fd_path })
}

/// Linux: a sealed anonymous `memfd`. `F_SEAL_WRITE` makes it genuinely
/// immutable (even scriptbox can no longer alter it) with no disk round-trip.
#[cfg(target_os = "linux")]
fn make_fd(bytes: &[u8]) -> Result<OwnedFd> {
    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
    let fd = memfd_create("scriptbox", MemfdFlags::ALLOW_SEALING).context("memfd_create")?;
    write_all(&fd, bytes)?;
    fcntl_add_seals(
        &fd,
        SealFlags::WRITE | SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL,
    )
    .context("sealing memfd (F_ADD_SEALS)")?;
    Ok(fd)
}

/// macOS/other POSIX: write to a private temp file, then re-open it read-only
/// and unlink the path. After the unlink no path reaches the bytes, only our
/// (read-only) fd, dup'd by the interpreter via `/dev/fd/N`, so a mid-run edit
/// to the original source cannot reach the running copy.
#[cfg(not(target_os = "linux"))]
fn make_fd(bytes: &[u8]) -> Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags, open, unlink};
    use std::path::PathBuf;

    let dir = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let pid = std::process::id();

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..128u32 {
        let path = dir.join(format!(".scriptbox.{pid}.{attempt}"));
        // O_EXCL: fail rather than open an attacker-planted file at this name.
        match open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(rw) => {
                write_all(&rw, bytes)?;
                drop(rw); // flush + close the writer
                // Fresh read-only open of the private path, then unlink it.
                let ro = open(&path, OFlags::RDONLY, Mode::empty())
                    .with_context(|| format!("re-open `{}` read-only", path.display()))?;
                unlink(&path).with_context(|| format!("unlink `{}`", path.display()))?;
                return Ok(ro);
            }
            Err(e) => last_err = Some(anyhow::Error::new(e)),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("could not create a temp copy")))
        .context("creating immutable temp copy")
}

fn write_all(fd: &OwnedFd, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let n = rustix::io::write(fd, bytes).context("writing script buffer")?;
        if n == 0 {
            anyhow::bail!("short write while buffering script");
        }
        bytes = &bytes[n..];
    }
    Ok(())
}

fn clear_cloexec(fd: &OwnedFd) -> Result<()> {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    let flags = fcntl_getfd(fd).context("fcntl F_GETFD")?;
    fcntl_setfd(fd, flags.difference(FdFlags::CLOEXEC)).context("fcntl F_SETFD")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn immutable_serves_the_exact_bytes() {
        let data = b"#!/bin/bash\necho hello\n";
        let s = immutable(data).unwrap();
        // The fd path (still open, since `s` is alive) reads back the bytes.
        assert!(s.fd_path.starts_with("/dev/fd/") || s.fd_path.starts_with("/proc/self/fd/"));
        assert_eq!(std::fs::read(&s.fd_path).unwrap(), data);
    }

    #[test]
    fn immutable_copy_rejects_writes() {
        // Sealed memfd (Linux) fails the write; a read-only-reopened + unlinked
        // temp (macOS) fails the open-for-write. Either way, no write lands.
        let s = immutable(b"frozen\n").unwrap();
        let wrote = std::fs::OpenOptions::new()
            .write(true)
            .open(&s.fd_path)
            .and_then(|mut f| f.write_all(b"x"));
        assert!(wrote.is_err(), "the immutable copy must reject writes");
        // ...and the original bytes are intact.
        assert_eq!(std::fs::read(&s.fd_path).unwrap(), b"frozen\n");
    }

    #[test]
    fn empty_script_is_handled() {
        let s = immutable(b"").unwrap();
        assert_eq!(std::fs::read(&s.fd_path).unwrap(), b"");
    }

    #[test]
    fn read_script_reads_a_normal_file() {
        let p = std::env::temp_dir().join(format!("scriptbox-rd.{}.sh", std::process::id()));
        std::fs::write(&p, b"#!/bin/sh\ntrue\n").unwrap();
        assert_eq!(read_script(&p).unwrap(), b"#!/bin/sh\ntrue\n");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_script_errors_on_a_missing_file() {
        let missing = std::env::temp_dir().join("scriptbox-does-not-exist.zzz.sh");
        assert!(read_script(&missing).is_err());
    }
}
