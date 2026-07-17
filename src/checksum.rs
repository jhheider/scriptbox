//! Optional integrity verification: a `sha256:<hex>` pin over the script bytes.
//!
//! This is a *separate* guarantee from runtime immutability - it answers "is
//! this the script I expect?" (provenance), not "can it change while running?".

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The canonical pin of a script: `sha256:<hex>` over the script's bytes *with
/// its entire `# /// scriptbox` frontmatter block excluded*.
///
/// The block is scriptbox's own config - interpreter, checksum, and
/// frontmatter-flippable switches - so it must not participate in the pin:
/// otherwise pinning would be circular (the checksum line), and flipping a
/// switch would spuriously break the pin. Everything else - the shebang and the
/// whole script body - still contributes, so tampering with what *runs* is
/// still caught. (Note: an interpreter set only in frontmatter is therefore not
/// covered by the pin; put it on the shebang line if you need it pinned.)
pub fn pin_of(bytes: &[u8]) -> String {
    sha256_pin(&digest_input(bytes))
}

/// Low-level `sha256:<hex>` over exactly the given bytes (no line exclusion).
pub fn sha256_pin(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(7 + digest.len() * 2);
    s.push_str("sha256:");
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Return `bytes` with the entire `# /// scriptbox` ... `# ///` block removed
/// (markers and terminators included). Only the first block is stripped.
/// Non-UTF-8 input is returned unchanged (a binary blob has no text block).
fn digest_input(bytes: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    let mut done = false; // only strip the first block
    for line in text.split_inclusive('\n') {
        let body = comment_body(line);
        if in_block {
            if body == Some("///") {
                in_block = false;
                done = true;
            }
            continue; // drop every line of the block, markers included
        }
        if !done && body == Some("/// scriptbox") {
            in_block = true;
            continue; // drop the opening marker
        }
        out.push_str(line);
    }
    out.into_bytes()
}

/// For a raw physical line, the trimmed body after stripping a leading `#` and
/// one optional space - or `None` if the line isn't a `#` comment.
fn comment_body(line: &str) -> Option<&str> {
    let after = line.trim_start().strip_prefix('#')?;
    Some(after.strip_prefix(' ').unwrap_or(after).trim())
}

/// Compare an expected pin against an actual pin, tolerantly: case-insensitive,
/// and accepting either `sha256:<hex>` or a bare `<hex>` on the expected side.
pub fn pins_match(expected: &str, actual_pin: &str) -> bool {
    let norm = |p: &str| -> String {
        let p = p.trim().to_ascii_lowercase();
        p.strip_prefix("sha256:").map(str::to_string).unwrap_or(p)
    };
    norm(expected) == norm(actual_pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_stable_and_prefixed() {
        // Known SHA-256 of the empty input.
        assert_eq!(
            sha256_pin(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_single_byte_change_changes_the_pin() {
        assert_ne!(sha256_pin(b"echo hi\n"), sha256_pin(b"echo ho\n"));
    }

    #[test]
    fn pin_is_invariant_to_the_whole_frontmatter_block() {
        // Any change *inside* the block - the checksum value (non-circular
        // pinning), the interpreter, a switch - must not change the pin.
        let a = b"#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:aaaa\"\n# ///\necho hi\n";
        let b = b"#!/bin/bash\n# /// scriptbox\n# interpreter = \"zsh\"\n# argv0 = \"source\"\n# checksum = \"sha256:bbbb\"\n# ///\necho hi\n";
        assert_eq!(pin_of(a), pin_of(b));
        // And both equal the pin with no block at all (same shebang + body).
        let none = b"#!/bin/bash\necho hi\n";
        assert_eq!(pin_of(a), pin_of(none));
    }

    #[test]
    fn pin_still_changes_when_real_content_changes() {
        let a = b"#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:x\"\n# ///\necho hi\n";
        let b =
            b"#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:x\"\n# ///\necho HImodified\n";
        assert_ne!(pin_of(a), pin_of(b));
    }

    #[test]
    fn matching_tolerates_prefix_and_case() {
        let pin = sha256_pin(b"deploy\n");
        let bare = pin.strip_prefix("sha256:").unwrap().to_uppercase();
        assert!(pins_match(&pin, &pin));
        assert!(pins_match(&bare, &pin)); // bare, upper-cased expected still matches
        assert!(!pins_match("sha256:deadbeef", &pin));
    }
}
