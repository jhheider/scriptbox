//! Optional integrity verification: a `sha256:<hex>` pin over the script bytes.
//!
//! This is a *separate* guarantee from runtime immutability - it answers "is
//! this the script I expect?" (provenance), not "can it change while running?".

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The canonical pin of a script: `sha256:<hex>` over the script's bytes *with
/// its own frontmatter `checksum` line excluded*.
///
/// Excluding that one line is what makes pinning non-circular - writing the pin
/// into the file doesn't change the value the pin is computed from - while every
/// other byte still contributes, so tampering is still caught.
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

/// Return `bytes` with the in-block frontmatter `checksum = ...` line removed
/// (line terminator included). Non-UTF-8 input is returned unchanged (a binary
/// blob has no text frontmatter to strip).
fn digest_input(bytes: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    let mut removed = false;
    for line in text.split_inclusive('\n') {
        let body = comment_body(line);
        if body == Some("/// scriptbox") {
            in_block = true;
        } else if in_block && body == Some("///") {
            in_block = false;
        } else if in_block && !removed && is_checksum_key(body) {
            removed = true;
            continue; // drop this physical line, terminator and all
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

fn is_checksum_key(body: Option<&str>) -> bool {
    body.and_then(|b| b.split_once('='))
        .is_some_and(|(k, _)| k.trim() == "checksum")
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
    fn pin_is_invariant_to_the_checksum_line_value() {
        // The whole point: changing only the stored checksum value must not
        // change the computed pin (otherwise pinning is circular).
        let a = b"#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:aaaa\"\n# ///\necho hi\n";
        let b = b"#!/bin/bash\n# /// scriptbox\n# checksum = \"sha256:bbbb\"\n# ///\necho hi\n";
        assert_eq!(pin_of(a), pin_of(b));
        // And it equals the pin of the same script with no checksum line at all.
        let none = b"#!/bin/bash\n# /// scriptbox\n# ///\necho hi\n";
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
