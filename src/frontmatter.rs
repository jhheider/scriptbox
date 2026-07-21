//! Inline `# /// scriptbox ... # ///` metadata, PEP-723 style.
//!
//! The block lets a script carry its interpreter and/or an expected checksum
//! without the shebang line having to spell them out, and because every line
//! is a `#` comment, the script still runs under a plain interpreter when
//! scriptbox isn't in the picture.
//!
//! ```text
//! #!/usr/bin/env scriptbox
//! # /// scriptbox
//! # interpreter = "bash"
//! # checksum = "sha256:..."
//! # ///
//! ```

/// The fields scriptbox understands from an inline block. Unknown keys are
/// ignored so the format can grow without breaking older binaries.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub interpreter: Option<String>,
    pub checksum: Option<String>,
    /// Raw switch values; resolved to enums (with error context) at run time.
    pub argv0: Option<String>,
    pub subscripts: Option<String>,
}

/// Parse the first `# /// scriptbox` block found in `bytes`. Returns an
/// all-`None` [`Frontmatter`] when no block is present.
pub fn parse(bytes: &[u8]) -> Frontmatter {
    let text = String::from_utf8_lossy(bytes);
    let mut fm = Frontmatter::default();
    let mut in_block = false;

    for raw in text.lines() {
        // Frontmatter lives entirely inside `#` comments; strip the `#` and one
        // optional following space to get the comment body.
        let Some(after_hash) = raw.trim_start().strip_prefix('#') else {
            continue;
        };
        let body = after_hash.strip_prefix(' ').unwrap_or(after_hash).trim();

        if body == "/// scriptbox" {
            in_block = true;
            continue;
        }
        if in_block && body == "///" {
            break;
        }
        if !in_block {
            continue;
        }
        if let Some((key, val)) = body.split_once('=') {
            let val = val.trim().trim_matches(['"', '\'']).to_string();
            match key.trim() {
                "interpreter" => fm.interpreter = Some(val),
                "checksum" => fm.checksum = Some(val),
                "argv0" => fm.argv0 = Some(val),
                "subscripts" => fm.subscripts = Some(val),
                _ => {}
            }
        }
    }
    fm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_interpreter_and_checksum() {
        let src = b"#!/usr/bin/env scriptbox\n\
                    # /// scriptbox\n\
                    # interpreter = \"bash\"\n\
                    # checksum = \"sha256:abc123\"\n\
                    # ///\n\
                    echo hi\n";
        let fm = parse(src);
        assert_eq!(fm.interpreter.as_deref(), Some("bash"));
        assert_eq!(fm.checksum.as_deref(), Some("sha256:abc123"));
    }

    #[test]
    fn no_block_is_all_none() {
        assert_eq!(parse(b"#!/bin/bash\necho hi\n"), Frontmatter::default());
    }

    #[test]
    fn stops_at_end_marker() {
        // A `checksum` after the closing `# ///` must not be picked up.
        let src = b"# /// scriptbox\n# interpreter = \"zsh\"\n# ///\n# checksum = \"nope\"\n";
        let fm = parse(src);
        assert_eq!(fm.interpreter.as_deref(), Some("zsh"));
        assert_eq!(fm.checksum, None);
    }
}
