//! Subscript analysis: find a script's child invocations - `source`/`.` includes
//! and interpreter calls (`bash x.sh`, `python foo.py`, `./run.sh`).
//!
//! In `wrap`/`freeze-tree` mode scriptbox routes the whole shell tree through
//! itself: subprocess children (`bash child.sh`, `./x.sh`) are rewritten to run
//! under scriptbox (so they're frozen recursively), and in-process `source`/`.`
//! of a resolvable file is frozen into an inherited immutable fd and rewritten
//! to `source /dev/fd/N` (so the included bytes can't change out from under the
//! caller either). Dynamic paths and already-immune interpreters (python/ruby/
//! node) are reported but left alone.
//!
//! The detector uses a real shell parser (`brush-parser`), a heavy dependency,
//! so it lives behind the non-default `subscripts` build feature.

use crate::config::Subscripts;
use crate::loader::ImmutableScript;
use anyhow::Result;
use std::path::Path;

/// The result of processing a script: the bytes to serve, plus any immutable fds
/// backing frozen `source` includes - which must be held open until `exec`.
pub struct Applied {
    pub bytes: Vec<u8>,
    pub held: Vec<ImmutableScript>,
}

/// Whether this binary was built with the shell parser (the `subscripts`
/// feature). All non-`Off` modes need it.
pub const fn enabled() -> bool {
    cfg!(feature = "subscripts")
}

/// Apply the requested subscript `mode` to `bytes`. Under `Wrap`/`FreezeTree`
/// the returned bytes have shell children routed through scriptbox and `source`
/// includes frozen; under `Report` they're unchanged. `cache_dir` is the
/// `freeze-tree` snapshot cache (or `None`).
pub fn apply(
    mode: Subscripts,
    bytes: &[u8],
    script: &Path,
    cache_dir: Option<&Path>,
) -> Result<Applied> {
    #[cfg(feature = "subscripts")]
    {
        detect::apply(mode, bytes, script, cache_dir)
    }
    #[cfg(not(feature = "subscripts"))]
    {
        let _ = (mode, bytes, script, cache_dir);
        anyhow::bail!(
            "subscript analysis was requested (--subscripts / `subscripts` frontmatter), \
             but this scriptbox was built without the `subscripts` feature.\n\
             Reinstall with it enabled:\n    \
             cargo install --features subscripts --git https://github.com/jhheider/scriptbox"
        )
    }
}

#[cfg(feature = "subscripts")]
mod detect {
    use super::Applied;
    use crate::config::Subscripts;
    use crate::interpreter::shell_squote;
    use crate::{cache, loader};
    use anyhow::{Context, Result};
    use brush_parser::ast;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[derive(PartialEq)]
    enum Category {
        Source,     // source / . - freeze the include into an fd, in-process
        ShellChild, // a shell interpreter call or a direct ./x.sh - run under scriptbox
        OtherChild, // python/ruby/node/... - already immune, not touched
    }

    struct Site {
        label: &'static str,
        category: Category,
        line: usize,
        /// Byte offset of the command word (where a subprocess wrap prefix goes).
        cmd_offset: usize,
        /// Byte span of the first path argument (where a source path is replaced).
        arg_span: Option<(usize, usize)>,
        target: String,
        resolvable: bool,
    }

    const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "ksh", "mksh"];
    const OTHER_INTERP: &[&str] = &["perl", "ruby", "node", "php", "lua"];
    const SHELL_EXTS: &[&str] = &[".sh", ".bash", ".zsh", ".ksh"];

    pub fn apply(
        mode: Subscripts,
        bytes: &[u8],
        script: &Path,
        cache_dir: Option<&Path>,
    ) -> Result<Applied> {
        let mut held = Vec::new();
        let mut visited = HashSet::new();
        if let Ok(c) = std::fs::canonicalize(script) {
            visited.insert(c);
        }
        let out = process(mode, bytes, script, cache_dir, &mut held, &mut visited)?;
        Ok(Applied { bytes: out, held })
    }

    /// Recursively rewrite `bytes`: prefix shell subprocess sites with a
    /// scriptbox invocation, and replace resolvable `source` paths with a frozen
    /// fd path (recursing into the sourced file first). `visited` breaks source
    /// cycles.
    fn process(
        mode: Subscripts,
        bytes: &[u8],
        script: &Path,
        cache_dir: Option<&Path>,
        held: &mut Vec<loader::ImmutableScript>,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<Vec<u8>> {
        let Ok(src) = std::str::from_utf8(bytes) else {
            eprintln!(
                "scriptbox: subscripts: {} is not UTF-8; skipping",
                script.display()
            );
            return Ok(bytes.to_vec());
        };
        let sites = match parse_sites(src) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "scriptbox: subscripts: could not parse {}: {e}",
                    script.display()
                );
                return Ok(bytes.to_vec());
            }
        };

        // report only?
        if mode == Subscripts::Report {
            print_report(script, &sites, false);
            return Ok(bytes.to_vec());
        }

        let exe = std::env::current_exe().context("finding the scriptbox binary")?;
        let exe = exe.to_string_lossy();
        let propagate = if mode == Subscripts::FreezeTree {
            "freeze-tree"
        } else {
            "wrap"
        };
        let wrap_prefix = format!("{} --subscripts={} ", shell_squote(&exe), propagate);

        // Collect edits (start, end, replacement); apply high offset -> low.
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for s in &sites {
            match s.category {
                Category::ShellChild if s.resolvable => {
                    edits.push((s.cmd_offset, s.cmd_offset, wrap_prefix.clone()));
                }
                Category::Source if s.resolvable => {
                    if let (Some((a, b)), Some(fd_path)) = (
                        s.arg_span,
                        freeze_source(mode, &s.target, cache_dir, held, visited)?,
                    ) {
                        edits.push((a, b, fd_path));
                    }
                }
                _ => {}
            }
        }

        print_report(script, &sites, true);

        edits.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
        let mut out = src.to_string();
        for (start, end, text) in edits {
            if out.is_char_boundary(start) && out.is_char_boundary(end) {
                out.replace_range(start..end, &text);
            }
        }
        Ok(out.into_bytes())
    }

    /// Freeze a resolvable `source` target into an immutable fd (after recursing
    /// into it) and return its fd path. `None` if the path can't be resolved to
    /// an existing file or would form a source cycle.
    fn freeze_source(
        mode: Subscripts,
        literal: &str,
        cache_dir: Option<&Path>,
        held: &mut Vec<loader::ImmutableScript>,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<Option<String>> {
        let Some(canonical) = resolve(literal) else {
            return Ok(None); // missing/unresolvable: leave the source as-is
        };
        if !visited.insert(canonical.clone()) {
            return Ok(None); // cycle: don't recurse; leave as-is
        }

        let disk = std::fs::read(&canonical)
            .with_context(|| format!("reading sourced file `{}`", canonical.display()))?;
        let bytes = match cache_dir {
            Some(c) => cache::frozen_bytes(c, &canonical, &disk)?,
            None => disk,
        };
        let processed = process(mode, &bytes, &canonical, cache_dir, held, visited)?;
        let imm = loader::immutable(&processed)?;
        let fd_path = imm.fd_path.clone();
        held.push(imm);
        Ok(Some(fd_path))
    }

    /// Resolve a literal `source` argument to an existing canonical path
    /// (relative to the current directory, matching how the shell resolves it).
    fn resolve(literal: &str) -> Option<PathBuf> {
        let p = Path::new(literal);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(p)
        };
        std::fs::canonicalize(abs).ok()
    }

    fn parse_sites(src: &str) -> Result<Vec<Site>> {
        let tokens = brush_parser::tokenize_str(src).map_err(|e| anyhow::anyhow!("{e}"))?;
        let program = brush_parser::parse_tokens(&tokens, &brush_parser::ParserOptions::default())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut cmds = Vec::new();
        for list in &program.complete_commands {
            walk_list(list, &mut cmds);
        }
        Ok(cmds.into_iter().filter_map(classify).collect())
    }

    fn classify(cmd: &ast::SimpleCommand) -> Option<Site> {
        let name_word = cmd.word_or_name.as_ref()?;
        let name = name_word.value.as_str();
        let loc = name_word.loc.as_ref();
        let line = loc.map(|s| s.start.line).unwrap_or(0);
        let cmd_offset = loc.map(|s| s.start.index).unwrap_or(0);
        let base = name.rsplit('/').next().unwrap_or(name);

        let args: Vec<&ast::Word> = cmd
            .suffix
            .as_ref()
            .map(|s| {
                s.0.iter()
                    .filter_map(|it| match it {
                        ast::CommandPrefixOrSuffixItem::Word(w) => Some(w),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let span_of = |w: &ast::Word| w.loc.as_ref().map(|s| (s.start.index, s.end.index));

        if name == "source" || name == "." {
            let t = args.first()?;
            return Some(Site {
                label: "source",
                category: Category::Source,
                line,
                cmd_offset,
                arg_span: span_of(t),
                target: t.value.clone(),
                resolvable: is_literal(&t.value),
            });
        }

        if SHELLS.contains(&base) || OTHER_INTERP.contains(&base) || base.starts_with("python") {
            if args.iter().any(|w| w.value == "-c") {
                return None; // inline `-c '...'`, no file child
            }
            let t = args.iter().find(|w| !w.value.starts_with('-'))?;
            let category = if SHELLS.contains(&base) {
                Category::ShellChild
            } else {
                Category::OtherChild
            };
            return Some(Site {
                label: interpreter_label(base),
                category,
                line,
                cmd_offset,
                arg_span: span_of(t),
                target: t.value.clone(),
                resolvable: is_literal(&t.value),
            });
        }

        if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
            let category = if SHELL_EXTS.iter().any(|e| name.ends_with(e)) {
                Category::ShellChild
            } else if [".py", ".pl", ".rb", ".js"]
                .iter()
                .any(|e| name.ends_with(e))
            {
                Category::OtherChild
            } else {
                return None;
            };
            return Some(Site {
                label: "exec",
                category,
                line,
                cmd_offset,
                arg_span: None,
                target: name.to_string(),
                resolvable: is_literal(name),
            });
        }
        None
    }

    fn is_literal(w: &str) -> bool {
        !w.contains('$') && !w.contains('`')
    }

    fn interpreter_label(base: &str) -> &'static str {
        match base {
            "bash" => "bash",
            "sh" => "sh",
            "dash" => "dash",
            "zsh" => "zsh",
            "ksh" | "mksh" => "ksh",
            "perl" => "perl",
            "ruby" => "ruby",
            "node" => "node",
            "php" => "php",
            "lua" => "lua",
            _ => "python",
        }
    }

    fn print_report(script: &Path, sites: &[Site], acting: bool) {
        if sites.is_empty() {
            return;
        }
        let verb = if acting { "wrapping" } else { "detection only" };
        eprintln!("scriptbox: subscripts in {} ({verb}):", script.display());
        for s in sites {
            let acted = matches!(
                (acting, &s.category, s.resolvable),
                (true, Category::ShellChild, true) | (true, Category::Source, true)
            );
            let status = if !acting {
                if s.resolvable {
                    "resolvable"
                } else {
                    "dynamic"
                }
            } else if acted {
                if s.category == Category::Source {
                    "frozen"
                } else {
                    "wrapped"
                }
            } else if !s.resolvable {
                "dynamic - left"
            } else if s.category == Category::OtherChild {
                "immune - left"
            } else {
                "left"
            };
            eprintln!(
                "  [{:<6}] line {:<4} {}  ({status})",
                s.label, s.line, s.target
            );
        }
    }

    // --- AST walk: collect every SimpleCommand, descending common nesters. ---

    fn walk_list<'a>(list: &'a ast::CompoundList, out: &mut Vec<&'a ast::SimpleCommand>) {
        for ast::CompoundListItem(and_or, _) in &list.0 {
            walk_pipeline(&and_or.first, out);
            for ao in &and_or.additional {
                match ao {
                    ast::AndOr::And(p) | ast::AndOr::Or(p) => walk_pipeline(p, out),
                }
            }
        }
    }

    fn walk_pipeline<'a>(p: &'a ast::Pipeline, out: &mut Vec<&'a ast::SimpleCommand>) {
        for cmd in &p.seq {
            walk_command(cmd, out);
        }
    }

    fn walk_command<'a>(cmd: &'a ast::Command, out: &mut Vec<&'a ast::SimpleCommand>) {
        match cmd {
            ast::Command::Simple(s) => out.push(s),
            ast::Command::Compound(cc, _) => walk_compound(cc, out),
            ast::Command::Function(f) => walk_compound(&f.body.0, out),
            ast::Command::ExtendedTest(..) => {}
        }
    }

    fn walk_compound<'a>(cc: &'a ast::CompoundCommand, out: &mut Vec<&'a ast::SimpleCommand>) {
        match cc {
            ast::CompoundCommand::BraceGroup(g) => walk_list(&g.list, out),
            ast::CompoundCommand::Subshell(s) => walk_list(&s.list, out),
            ast::CompoundCommand::ForClause(f) => walk_list(&f.body.list, out),
            ast::CompoundCommand::WhileClause(w) | ast::CompoundCommand::UntilClause(w) => {
                walk_list(&w.0, out);
                walk_list(&w.1.list, out);
            }
            ast::CompoundCommand::IfClause(i) => {
                walk_list(&i.condition, out);
                walk_list(&i.then, out);
                if let Some(elses) = &i.elses {
                    for e in elses {
                        if let Some(c) = &e.condition {
                            walk_list(c, out);
                        }
                        walk_list(&e.body, out);
                    }
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classify_finds_source_arg_span_and_shell_children() {
            let src = "#!/bin/bash\nsource ./lib.sh\nbash child.sh\npython x.py\nsource \"$D/y\"\n";
            let sites = parse_sites(src).unwrap();
            let srcs: Vec<_> = sites
                .iter()
                .filter(|s| s.category == Category::Source)
                .collect();
            assert_eq!(srcs.len(), 2);
            assert!(srcs[0].resolvable && srcs[0].arg_span.is_some());
            assert!(!srcs[1].resolvable); // "$D/y" is dynamic
            assert!(
                sites
                    .iter()
                    .any(|s| s.category == Category::ShellChild && s.resolvable)
            );
            assert!(sites.iter().any(|s| s.category == Category::OtherChild)); // python
        }
    }
}
