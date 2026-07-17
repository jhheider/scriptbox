//! The `pin` and `hash` subcommands — for creating checksum-pinned invocations.

use anyhow::Result;
use std::path::Path;

use crate::{checksum, loader};

/// Print just the canonical `sha256:<hex>` pin of a script.
pub fn hash(path: &Path) -> Result<()> {
    let bytes = loader::read_script(path)?;
    println!("{}", checksum::pin_of(&bytes));
    Ok(())
}

/// Print a ready-to-paste `checksum` frontmatter line pinning the script's
/// current bytes. Because the pin excludes the checksum line itself, pasting
/// this line and re-running `pin` yields the same value (no chasing a fixpoint).
pub fn pin(path: &Path) -> Result<()> {
    let bytes = loader::read_script(path)?;
    let pin = checksum::pin_of(&bytes);
    let fm = crate::frontmatter::parse(&bytes);
    if fm.interpreter.is_none() && fm.checksum.is_none() {
        // No block yet — emit a complete one.
        println!("# /// scriptbox");
        println!("# checksum = \"{pin}\"");
        println!("# ///");
    } else {
        // A block already exists — just the line to drop into it.
        println!("# checksum = \"{pin}\"");
    }
    Ok(())
}
