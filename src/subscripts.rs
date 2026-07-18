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

/// A child-script site of a script, for `emit --subscripts` tree inspection.
/// Only exists on a build with the shell parser (the `subscripts` feature).
#[cfg(feature = "subscripts")]
pub enum Child {
    /// A resolvable child script path (a `source` include or a shell child).
    Resolved(std::path::PathBuf),
    /// A site we can't statically follow (a dynamic path) or won't (an immune
    /// interpreter), with a human description for the dump.
    Note(String),
}

/// Enumerate the child-script sites of `bytes` (the script at `script`):
/// resolvable `source` includes and shell children as paths, dynamic/immune ones
/// as notes. Static and read-only; the order matches the sites' order in the file.
#[cfg(feature = "subscripts")]
pub fn child_scripts(bytes: &[u8], script: &Path) -> Vec<Child> {
    detect::child_scripts(bytes, script)
}

#[cfg(feature = "subscripts")]
mod detect {
    use super::Applied;
    use crate::config::Subscripts;
    use crate::interpreter::shell_squote;
    use crate::{cache, loader};
    use anyhow::{Context, Result};
    use brush_parser::ast;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    /// Cap on recursive `source` freezing depth (fd/stack backstop).
    const SOURCE_DEPTH_CAP: usize = 64;

    #[derive(PartialEq, Clone, Copy)]
    enum Category {
        Source,     // source / . - freeze the include into an fd, in-process
        ShellChild, // a shell interpreter call or a direct ./x.sh - run under scriptbox
        OtherChild, // python/ruby/node/... - already immune, not touched
    }

    struct Site {
        label: &'static str,
        category: Category,
        line: usize,
        cmd_offset: usize,                // command-word start (subprocess prefix)
        arg_span: Option<(usize, usize)>, // path-arg span (source replacement)
        target: String,                   // dequoted
        resolvable: bool,
    }

    /// Recursion state threaded through the whole tree of a single invocation.
    struct Ctx<'a> {
        mode: Subscripts,
        cache_dir: Option<&'a Path>,
        held: Vec<loader::ImmutableScript>,
        frozen: HashMap<PathBuf, String>, // canonical path -> its frozen fd path (dedup)
        in_progress: HashSet<PathBuf>,    // canonical paths being frozen now (cycle guard)
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
        let canonical = std::fs::canonicalize(script).unwrap_or_else(|_| script.to_path_buf());
        let mut ctx = Ctx {
            mode,
            cache_dir,
            held: Vec::new(),
            frozen: HashMap::new(),
            in_progress: HashSet::new(),
        };
        // The top-level script must not be re-frozen if something sources it back.
        ctx.in_progress.insert(canonical.clone());
        let out = process(&mut ctx, bytes, &canonical, 0)?;
        Ok(Applied {
            bytes: out,
            held: ctx.held,
        })
    }

    /// Rewrite `bytes` (the script at canonical `script`): prefix shell
    /// subprocess sites with a scriptbox invocation, and replace resolvable
    /// `source` paths with a frozen fd path (recursing into the sourced file).
    fn process(ctx: &mut Ctx, bytes: &[u8], script: &Path, depth: usize) -> Result<Vec<u8>> {
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

        if ctx.mode == Subscripts::Report {
            report_detection(script, &sites);
            return Ok(bytes.to_vec());
        }

        let exe = std::env::current_exe().context("finding the scriptbox binary")?;
        let exe = exe.to_string_lossy();
        // One protective mode; children inherit it (and the shared cache via env).
        let wrap_prefix = format!("{} --subscripts=freeze ", shell_squote(&exe));

        // Build edits and the per-site outcome IN LOCKSTEP, so the report never
        // claims a site was frozen/wrapped when no edit was actually made.
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut outcomes: Vec<&'static str> = Vec::with_capacity(sites.len());
        for s in &sites {
            let outcome = match s.category {
                Category::ShellChild if s.resolvable => {
                    edits.push((s.cmd_offset, s.cmd_offset, wrap_prefix.clone()));
                    "wrapped"
                }
                Category::Source if s.resolvable => {
                    match freeze_source(ctx, &s.target, script, depth)? {
                        Some(fd) if s.arg_span.is_some() => {
                            let (a, b) = s.arg_span.unwrap();
                            edits.push((a, b, fd));
                            "frozen"
                        }
                        _ => "left - unresolved/ambiguous",
                    }
                }
                _ if !s.resolvable => "left - dynamic",
                Category::OtherChild => "left - immune",
                _ => "left",
            };
            outcomes.push(outcome);
        }

        report_outcomes(script, &sites, &outcomes);

        edits.sort_unstable_by_key(|e| std::cmp::Reverse(e.0));
        let mut out = src.to_string();
        for (start, end, text) in edits {
            if out.is_char_boundary(start) && out.is_char_boundary(end) {
                out.replace_range(start..end, &text);
            }
        }
        Ok(out.into_bytes())
    }

    /// Freeze a resolvable `source` target into an immutable fd and return its fd
    /// path. Reuses an already-frozen file's fd (dedup); returns `None` for an
    /// unresolved/ambiguous path, a genuine source cycle, or past the depth cap.
    fn freeze_source(
        ctx: &mut Ctx,
        literal: &str,
        from: &Path,
        depth: usize,
    ) -> Result<Option<String>> {
        let Some(canonical) = resolve(literal, from) else {
            return Ok(None); // missing, or ambiguous between script-dir and CWD
        };
        if let Some(fd) = ctx.frozen.get(&canonical) {
            return Ok(Some(fd.clone())); // already frozen once - reuse the same fd
        }
        if ctx.in_progress.contains(&canonical) {
            return Ok(None); // genuine cycle: leave it, the shell will loop as written
        }
        if depth >= SOURCE_DEPTH_CAP {
            eprintln!(
                "scriptbox: subscripts: source depth cap ({SOURCE_DEPTH_CAP}) hit at {}; leaving it",
                canonical.display()
            );
            return Ok(None);
        }

        ctx.in_progress.insert(canonical.clone());
        let disk = std::fs::read(&canonical)
            .with_context(|| format!("reading sourced file `{}`", canonical.display()))?;
        let bytes = match ctx.cache_dir {
            Some(c) => cache::frozen_bytes(c, &canonical, &disk)?,
            None => disk,
        };
        let processed = process(ctx, &bytes, &canonical, depth + 1)?;
        let imm = loader::immutable(&processed)?;
        let fd_path = imm.fd_path.clone();
        ctx.held.push(imm);
        ctx.in_progress.remove(&canonical);
        ctx.frozen.insert(canonical, fd_path.clone());
        Ok(Some(fd_path))
    }

    /// Resolve a literal `source` argument to an existing canonical path. Tries
    /// both the sourcing script's own directory and the current directory (a
    /// shell resolves against runtime CWD, which we can't know statically). To
    /// avoid silently freezing the WRONG file, it only commits when the
    /// candidates agree on exactly one file; a disagreement returns `None`.
    fn resolve(literal: &str, from: &Path) -> Option<PathBuf> {
        let p = Path::new(literal);
        if p.is_absolute() {
            return std::fs::canonicalize(p).ok();
        }
        let mut hits: HashSet<PathBuf> = HashSet::new();
        if let Some(dir) = from.parent() {
            if let Ok(c) = std::fs::canonicalize(dir.join(p)) {
                hits.insert(c);
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(c) = std::fs::canonicalize(cwd.join(p)) {
                hits.insert(c);
            }
        }
        if hits.len() == 1 {
            hits.into_iter().next()
        } else {
            None
        }
    }

    /// Static child-site enumeration for `emit --subscripts` (see the module-level
    /// wrapper). Resolvable `source`/shell-child paths become `Resolved`; dynamic
    /// or immune sites become a descriptive `Note`.
    pub fn child_scripts(bytes: &[u8], script: &Path) -> Vec<super::Child> {
        use super::Child;
        let Ok(src) = std::str::from_utf8(bytes) else {
            return Vec::new();
        };
        let Ok(sites) = parse_sites(src) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for s in &sites {
            let resolved = if s.resolvable && s.category != Category::OtherChild {
                resolve(&s.target, script)
            } else {
                None
            };
            match resolved {
                Some(p) => out.push(Child::Resolved(p)),
                None => {
                    let why = if s.category == Category::OtherChild {
                        "immune interpreter"
                    } else {
                        "dynamic / unresolvable path"
                    };
                    out.push(Child::Note(format!(
                        "line {}: {} {} - {}",
                        s.line, s.label, s.target, why
                    )));
                }
            }
        }
        out
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

        // Dequote a Word's value (brush keeps surrounding quotes in `.value`), so
        // `source "./lib.sh"` resolves and `"$D/x"` still reads as dynamic.
        let deq = |w: &ast::Word| brush_parser::unquote_str(&w.value);
        let span_of = |w: &ast::Word| w.loc.as_ref().map(|s| (s.start.index, s.end.index));

        if name == "source" || name == "." {
            let t = args.first()?;
            let target = deq(t);
            let resolvable = is_literal(&target);
            return Some(Site {
                label: "source",
                category: Category::Source,
                line,
                cmd_offset,
                arg_span: span_of(t),
                target,
                resolvable,
            });
        }

        if SHELLS.contains(&base) || OTHER_INTERP.contains(&base) || base.starts_with("python") {
            if args.iter().any(|w| w.value == "-c") {
                return None; // inline `-c '...'`, no file child
            }
            let t = args.iter().find(|w| !w.value.starts_with('-'))?;
            let target = deq(t);
            let resolvable = is_literal(&target);
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
                target,
                resolvable,
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

    /// A word is statically resolvable if (after dequoting) it carries no shell
    /// expansion.
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

    fn report_detection(script: &Path, sites: &[Site]) {
        if sites.is_empty() {
            return;
        }
        eprintln!(
            "scriptbox: subscripts in {} (detection only):",
            script.display()
        );
        for s in sites {
            let status = if s.resolvable {
                "resolvable"
            } else {
                "dynamic"
            };
            eprintln!(
                "  [{:<6}] line {:<4} {}  ({status})",
                s.label, s.line, s.target
            );
        }
    }

    fn report_outcomes(script: &Path, sites: &[Site], outcomes: &[&str]) {
        if sites.is_empty() {
            return;
        }
        eprintln!("scriptbox: subscripts in {} (wrapping):", script.display());
        for (s, status) in sites.iter().zip(outcomes) {
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
            ast::CompoundCommand::CaseClause(c) => {
                for item in &c.cases {
                    if let Some(list) = &item.cmd {
                        walk_list(list, out);
                    }
                }
            }
            ast::CompoundCommand::ArithmeticForClause(f) => walk_list(&f.body.list, out),
            // Coprocess bodies and command-substitution interiors aren't
            // descended (brush's Word carries only raw text); such sites are
            // simply not seen.
            _ => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn dequotes_and_classifies() {
            // Quoted literal source resolves; a variable path stays dynamic; a
            // source inside `case` is still seen.
            let src = "#!/bin/bash\n\
                       source \"./lib.sh\"\n\
                       source \"$D/y\"\n\
                       case $1 in a) source ./inner.sh ;; esac\n";
            let sites = parse_sites(src).unwrap();
            let srcs: Vec<_> = sites
                .iter()
                .filter(|s| s.category == Category::Source)
                .collect();
            assert_eq!(srcs.len(), 3, "case-branch source must be seen");
            assert_eq!(srcs[0].target, "./lib.sh"); // dequoted
            assert!(srcs[0].resolvable);
            assert!(!srcs[1].resolvable); // "$D/y" -> $D/y -> dynamic
            assert!(srcs[2].resolvable && srcs[2].target == "./inner.sh");
        }
    }
}
