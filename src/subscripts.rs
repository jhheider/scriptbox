//! Subscript analysis: find a script's child invocations - `source`/`.` includes
//! and interpreter calls (`bash x.sh`, `python foo.py`, `./run.sh`) - and, in
//! `wrap` mode, route the *shell* children through scriptbox so they're frozen
//! too (recursively). In `report` mode it just prints what it found.
//!
//! The detector uses a real shell parser (`brush-parser`), a heavy dependency,
//! so it lives behind the non-default `subscripts` build feature. A lean default
//! build omits it entirely, and requesting analysis then errors clearly.

use crate::config::Subscripts;
use anyhow::Result;
use std::path::Path;

/// Apply the requested subscript `mode` to `bytes`, returning the bytes to serve
/// the interpreter (rewritten under `Wrap`, unchanged under `Report`). Never
/// called with `Subscripts::Off`.
pub fn apply(mode: Subscripts, bytes: &[u8], script: &Path) -> Result<Vec<u8>> {
    #[cfg(feature = "subscripts")]
    {
        detect::apply(mode, bytes, script)
    }
    #[cfg(not(feature = "subscripts"))]
    {
        let _ = (mode, bytes, script);
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
    use crate::config::Subscripts;
    use crate::interpreter::shell_squote;
    use anyhow::{Context, Result};
    use brush_parser::ast;
    use std::path::Path;

    #[derive(PartialEq)]
    enum Category {
        Source,     // source / . - runs in-process, can't be spawned through scriptbox
        ShellChild, // a shell interpreter call or a direct ./x.sh - wrappable
        OtherChild, // python/ruby/node/... - already immune, not wrapped
    }

    struct Site {
        label: &'static str,
        category: Category,
        line: usize,
        /// Byte offset of the command word (where a wrap prefix is inserted).
        offset: usize,
        target: String,
        resolvable: bool,
    }

    impl Site {
        fn wrappable(&self) -> bool {
            self.category == Category::ShellChild && self.resolvable
        }
        fn skip_reason(&self) -> &'static str {
            if !self.resolvable {
                "dynamic path"
            } else {
                match self.category {
                    Category::Source => "in-process source",
                    Category::OtherChild => "already immune",
                    Category::ShellChild => "",
                }
            }
        }
    }

    const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "ksh", "mksh"];
    const OTHER_INTERP: &[&str] = &["perl", "ruby", "node", "php", "lua"];
    const SHELL_EXTS: &[&str] = &[".sh", ".bash", ".zsh", ".ksh"];

    pub fn apply(mode: Subscripts, bytes: &[u8], script: &Path) -> Result<Vec<u8>> {
        let Ok(src) = std::str::from_utf8(bytes) else {
            eprintln!(
                "scriptbox: subscripts: {} is not UTF-8; skipping analysis",
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
                return Ok(bytes.to_vec()); // never block the run on a parse hiccup
            }
        };

        match mode {
            Subscripts::Report => {
                print_report(script, &sites, false);
                Ok(bytes.to_vec())
            }
            Subscripts::Wrap => {
                let exe =
                    std::env::current_exe().context("finding the scriptbox binary to wrap with")?;
                let exe = exe.to_string_lossy();
                let out = wrap(src, &sites, &exe);
                print_report(script, &sites, true);
                Ok(out.into_bytes())
            }
            Subscripts::Off => Ok(bytes.to_vec()),
        }
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

    /// Insert `scriptbox --subscripts=wrap ` before each wrappable child command,
    /// applied high offset to low so earlier offsets stay valid.
    fn wrap(src: &str, sites: &[Site], exe: &str) -> String {
        let prefix = format!("{} --subscripts=wrap ", shell_squote(exe));
        let mut offsets: Vec<usize> = sites
            .iter()
            .filter(|s| s.wrappable())
            .map(|s| s.offset)
            .collect();
        offsets.sort_unstable_by(|a, b| b.cmp(a));
        let mut out = src.to_string();
        for off in offsets {
            if out.is_char_boundary(off) {
                out.insert_str(off, &prefix);
            }
        }
        out
    }

    fn classify(cmd: &ast::SimpleCommand) -> Option<Site> {
        let name_word = cmd.word_or_name.as_ref()?;
        let name = name_word.value.as_str();
        let loc = name_word.loc.as_ref();
        let line = loc.map(|s| s.start.line).unwrap_or(0);
        let offset = loc.map(|s| s.start.index).unwrap_or(0);
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

        let site = |label, category, target: &str| Site {
            label,
            category,
            line,
            offset,
            target: target.to_string(),
            resolvable: is_literal(target),
        };

        if name == "source" || name == "." {
            let t = args.first()?;
            return Some(site("source", Category::Source, &t.value));
        }

        if SHELLS.contains(&base) || OTHER_INTERP.contains(&base) || base.starts_with("python") {
            if args.iter().any(|w| w.value == "-c") {
                return None; // inline `-c '...'`, no file child
            }
            let t = args.iter().find(|w| !w.value.starts_with('-'))?;
            let cat = if SHELLS.contains(&base) {
                Category::ShellChild
            } else {
                Category::OtherChild
            };
            return Some(site(interpreter_label(base), cat, &t.value));
        }

        // A path executed directly: `./run.sh`, `/opt/tools.py`.
        if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
            if SHELL_EXTS.iter().any(|e| name.ends_with(e)) {
                return Some(site("exec", Category::ShellChild, name));
            }
            if [".py", ".pl", ".rb", ".js"]
                .iter()
                .any(|e| name.ends_with(e))
            {
                return Some(site("exec", Category::OtherChild, name));
            }
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

    fn print_report(script: &Path, sites: &[Site], wrapping: bool) {
        if sites.is_empty() {
            eprintln!(
                "scriptbox: subscripts: no child invocations in {}",
                script.display()
            );
            return;
        }
        let verb = if wrapping {
            "wrapping"
        } else {
            "detection only"
        };
        eprintln!("scriptbox: subscripts in {} ({verb}):", script.display());
        for s in sites {
            let status = if wrapping && s.wrappable() {
                "wrapped".to_string()
            } else if wrapping {
                format!("left - {}", s.skip_reason())
            } else if s.resolvable {
                "resolvable".to_string()
            } else {
                "dynamic".to_string()
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
            // Case / arithmetic / coproc: not descended in the spike.
            _ => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wrap_rewrites_shell_children_but_not_source_or_immune_or_dynamic() {
            let src = "#!/bin/bash\n\
                       source ./lib.sh\n\
                       bash child.sh a\n\
                       python foo.py\n\
                       ./step.sh\n\
                       bash \"$X\"\n";
            let sites = parse_sites(src).unwrap();
            let out = wrap(src, &sites, "/usr/local/bin/scriptbox");
            // Shell subprocess + shell exec are wrapped (exe is shell-quoted).
            assert!(
                out.contains("scriptbox' --subscripts=wrap bash child.sh"),
                "got:\n{out}"
            );
            assert!(
                out.contains("scriptbox' --subscripts=wrap ./step.sh"),
                "got:\n{out}"
            );
            // ...source, immune python, and dynamic bash are left alone.
            assert!(out.contains("\nsource ./lib.sh\n"));
            assert!(out.contains("\npython foo.py\n"));
            assert!(out.contains("bash \"$X\"") && !out.contains("wrap bash \"$X\""));
        }

        #[test]
        fn inline_dash_c_is_not_a_child() {
            let sites = parse_sites("bash -c 'echo hi'\n").unwrap();
            assert!(sites.is_empty());
        }
    }
}
