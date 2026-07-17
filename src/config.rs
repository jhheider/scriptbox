//! Toggleable switches. Each is settable two ways with the same vocabulary - a
//! CLI flag or a `# /// scriptbox` frontmatter key - and CLI wins over
//! frontmatter wins over the default. Some fixes conflict (you can't rewrite
//! `$0` in-run *and* dot-source it), so each conflicting set is one switch with
//! named modes rather than a pile of booleans.

use anyhow::{Result, bail};

/// How scriptbox makes `$0` report the real script path. The interpreter reads
/// from an fd path, so without help `$0` shows that fd path. These modes
/// conflict, hence one switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argv0 {
    /// In-run reset where the shell supports it (bash >= 5 `BASH_ARGV0`, zsh
    /// `0=`); a harmless no-op on dash/ksh/bash-3.2. Preserves run-mode
    /// semantics and correct line numbers. (default)
    Rewrite,
    /// Universal dot-source: `<sh> -c '. <fd> "$@"' <realpath> args`. Gives the
    /// real `$0` on *every* POSIX shell, at the cost of sourced-mode semantics
    /// (top-level `return` becomes legal; the sourced-or-executed idiom flips).
    Source,
    /// Leave `$0` as the fd path; rely on `$SCRIPTBOX_SOURCE`.
    Off,
}

impl Argv0 {
    pub const DEFAULT: Argv0 = Argv0::Rewrite;

    pub fn parse(s: &str) -> Result<Argv0> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "rewrite" => Argv0::Rewrite,
            "source" => Argv0::Source,
            "off" | "none" | "false" => Argv0::Off,
            other => bail!("unknown argv0 mode `{other}` (want: rewrite | source | off)"),
        })
    }
}

/// Whether to analyze the script's child invocations (`source`/`.` and
/// interpreter calls). Opt-in, and the detector itself is behind the
/// `subscripts` build feature - a lean default build omits it entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subscripts {
    /// No subscript analysis. (default)
    Off,
    /// Statically detect and report resolvable `source`/interpreter call sites.
    /// Detection only - it does not touch what runs.
    Report,
    /// Rewrite resolvable *shell* child invocations (`bash child.sh`, `./x.sh`)
    /// to route through scriptbox, so each child is frozen too (recursively).
    /// `source`/`.` (in-process), dynamic paths, and already-immune interpreters
    /// (python/ruby/node) are reported but left unwrapped.
    Wrap,
}

impl Subscripts {
    pub const DEFAULT: Subscripts = Subscripts::Off;

    pub fn parse(s: &str) -> Result<Subscripts> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => Subscripts::Off,
            "report" | "on" | "true" => Subscripts::Report,
            "wrap" => Subscripts::Wrap,
            other => bail!("unknown subscripts mode `{other}` (want: off | report | wrap)"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_parsing() {
        assert_eq!(Argv0::parse("source").unwrap(), Argv0::Source);
        assert_eq!(Argv0::parse("REWRITE").unwrap(), Argv0::Rewrite);
        assert_eq!(Argv0::parse("off").unwrap(), Argv0::Off);
        assert!(Argv0::parse("wat").is_err());
    }

    #[test]
    fn subscripts_parsing() {
        assert_eq!(Subscripts::parse("report").unwrap(), Subscripts::Report);
        assert_eq!(Subscripts::parse("off").unwrap(), Subscripts::Off);
        assert!(Subscripts::parse("freeze").is_err());
    }
}
