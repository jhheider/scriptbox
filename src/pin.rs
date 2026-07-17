//! The `pin` and `hash` subcommands - for creating checksum-pinned invocations.

use anyhow::Result;
use std::path::Path;

use crate::{checksum, frontmatter, loader};

/// Print just the canonical `sha256:<hex>` pin of a script.
pub fn hash(path: &Path) -> Result<()> {
    let bytes = loader::read_script(path)?;
    println!("{}", checksum::pin_of(&bytes));
    Ok(())
}

/// Print a ready-to-paste `checksum` frontmatter line pinning the script's
/// current bytes. Because the pin excludes the checksum line itself, pasting
/// this and re-running `pin` yields the same value (no chasing a fixpoint).
pub fn pin(path: &Path) -> Result<()> {
    let bytes = loader::read_script(path)?;
    print!("{}", pin_block(&bytes));
    Ok(())
}

/// The text `pin` prints: a whole `# /// scriptbox` block when the script has no
/// block yet, or just the `checksum` line to drop into an existing one.
fn pin_block(bytes: &[u8]) -> String {
    let pin = checksum::pin_of(bytes);
    let fm = frontmatter::parse(bytes);
    if fm.interpreter.is_none() && fm.checksum.is_none() {
        format!("# /// scriptbox\n# checksum = \"{pin}\"\n# ///\n")
    } else {
        format!("# checksum = \"{pin}\"\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_a_full_block_when_none_exists() {
        let out = pin_block(b"#!/bin/bash\necho hi\n");
        assert!(out.starts_with("# /// scriptbox\n# checksum = \"sha256:"));
        assert!(out.ends_with("\"\n# ///\n"));
    }

    #[test]
    fn emits_just_the_line_when_a_block_exists() {
        let out =
            pin_block(b"#!/bin/bash\n# /// scriptbox\n# interpreter = \"bash\"\n# ///\necho hi\n");
        assert!(out.starts_with("# checksum = \"sha256:"));
        assert!(!out.contains("/// scriptbox"));
    }

    #[test]
    fn the_emitted_pin_is_a_fixpoint() {
        // Pasting pin_block's line into the file yields the same pin next time.
        let src = "#!/bin/bash\n# /// scriptbox\n# checksum = \"PLACEHOLDER\"\n# ///\necho hi\n";
        let pin = checksum::pin_of(src.as_bytes());
        let pinned = src.replace("PLACEHOLDER", &pin);
        assert!(pin_block(pinned.as_bytes()).contains(&pin));
    }

    #[test]
    fn hash_and_pin_read_a_file_and_succeed() {
        let p = std::env::temp_dir().join(format!("scriptbox-pinio.{}.sh", std::process::id()));
        std::fs::write(&p, b"#!/bin/bash\necho hi\n").unwrap();
        assert!(hash(&p).is_ok());
        assert!(pin(&p).is_ok());
        assert!(hash(Path::new("/no/such/scriptbox/file")).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
