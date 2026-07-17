//! Subscript analysis (spike): statically find a script's child invocations -
//! `source`/`.` includes and interpreter calls (`bash x.sh`, `python foo.py`,
//! `./run.sh`) - so scriptbox can eventually extend immutability to the "tree of
//! scripts." This is REPORT-ONLY: it detects and prints resolvable call sites;
//! it does not yet freeze the children.
//!
//! The detector uses a real shell parser (`brush-parser`), which is a heavy
//! dependency, so it lives behind the non-default `subscripts` build feature. A
//! lean default build omits it entirely, and requesting analysis then errors
//! with a clear message rather than silently doing nothing.

use anyhow::Result;
use std::path::Path;

/// Report statically-resolvable child-script invocations found in `bytes`.
pub fn report(bytes: &[u8], script: &Path) -> Result<()> {
    #[cfg(feature = "subscripts")]
    {
        detect::report(bytes, script)
    }
    #[cfg(not(feature = "subscripts"))]
    {
        let _ = (bytes, script);
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
    use anyhow::Result;
    use brush_parser::ast;
    use std::path::Path;

    /// A detected child-script invocation.
    struct Site {
        kind: &'static str, // "source" | interpreter name | "exec"
        line: usize,
        target: String,
        resolvable: bool, // literal path (vs. contains an expansion)
    }

    const INTERPRETERS: &[&str] = &[
        "bash", "sh", "dash", "zsh", "ksh", "mksh", "perl", "ruby", "node", "php", "lua",
    ];

    pub fn report(bytes: &[u8], script: &Path) -> Result<()> {
        let src = String::from_utf8_lossy(bytes);
        let tokens = match brush_parser::tokenize_str(&src) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "scriptbox: subscripts: could not tokenize {}: {e}",
                    script.display()
                );
                return Ok(()); // report-only; never block the run on a parse hiccup
            }
        };
        let program =
            match brush_parser::parse_tokens(&tokens, &brush_parser::ParserOptions::default()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "scriptbox: subscripts: could not parse {}: {e}",
                        script.display()
                    );
                    return Ok(());
                }
            };

        let mut cmds = Vec::new();
        for list in &program.complete_commands {
            walk_list(list, &mut cmds);
        }

        let mut sites = Vec::new();
        for c in cmds {
            if let Some(site) = classify(c) {
                sites.push(site);
            }
        }

        print_report(script, &sites);
        Ok(())
    }

    fn classify(cmd: &ast::SimpleCommand) -> Option<Site> {
        let name_word = cmd.word_or_name.as_ref()?;
        let name = name_word.value.as_str();
        let line = name_word.loc.as_ref().map(|s| s.start.line).unwrap_or(0);
        let base = name.rsplit('/').next().unwrap_or(name);

        // Suffix words, in order (skipping redirects/assignments).
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

        if name == "source" || name == "." {
            let target = args.first()?;
            return Some(Site {
                kind: "source",
                line,
                target: target.value.clone(),
                resolvable: is_literal(&target.value),
            });
        }

        if INTERPRETERS.contains(&base) || base.starts_with("python") {
            // `-c '...'` runs an inline string, not a file child: skip.
            if args.iter().any(|w| w.value == "-c") {
                return None;
            }
            let target = args.iter().find(|w| !w.value.starts_with('-'))?;
            return Some(Site {
                kind: interpreter_label(base),
                line,
                target: target.value.clone(),
                resolvable: is_literal(&target.value),
            });
        }

        // A bare path to a script executed directly: `./run.sh`, `/opt/x.sh`.
        if (name.starts_with("./") || name.starts_with("../") || name.starts_with('/'))
            && looks_scripty(name)
        {
            return Some(Site {
                kind: "exec",
                line,
                target: name.to_string(),
                resolvable: is_literal(name),
            });
        }
        None
    }

    /// A word is statically resolvable if it carries no shell expansion.
    fn is_literal(w: &str) -> bool {
        !w.contains('$') && !w.contains('`')
    }

    fn looks_scripty(p: &str) -> bool {
        [".sh", ".bash", ".zsh", ".ksh", ".py", ".pl", ".rb", ".js"]
            .iter()
            .any(|ext| p.ends_with(ext))
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

    fn print_report(script: &Path, sites: &[Site]) {
        if sites.is_empty() {
            eprintln!(
                "scriptbox: subscripts: no child invocations found in {} (spike; detection only)",
                script.display()
            );
            return;
        }
        eprintln!(
            "scriptbox: subscripts in {} (spike; detection only, nothing is frozen):",
            script.display()
        );
        let mut dynamic = 0;
        for s in sites {
            let note = if s.resolvable {
                "resolvable"
            } else {
                dynamic += 1;
                "dynamic - needs a directive or runtime trace"
            };
            eprintln!(
                "  [{:<7}] line {:<4} {}  ({note})",
                s.kind, s.line, s.target
            );
        }
        if dynamic > 0 {
            eprintln!(
                "  {dynamic} dynamic site(s) can't be resolved statically (paths built from \
                 variables/command-substitution)."
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
}
