//! scriptbox - read a script fully into an immutable copy at invoke, verify an
//! optional checksum, then hand it to the real interpreter. Closes the window
//! where editing a running script (by you, a background process, or malware)
//! changes what executes past the current line.

mod checksum;
mod frontmatter;
mod interpreter;
mod loader;
mod pin;
mod run;
mod shebang;

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What an argument vector resolves to.
enum Action {
    Run(run::RunSpec),
    Hash(PathBuf),
    Pin(PathBuf),
    Version,
    Help,
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("scriptbox: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args)? {
        Action::Version => println!("scriptbox {VERSION}"),
        Action::Help => usage(),
        Action::Hash(p) => pin::hash(&p)?,
        Action::Pin(p) => pin::pin(&p)?,
        // On success `run` never returns (it execs); it only returns `Err`.
        Action::Run(spec) => match run::run(spec)? {},
    }
    Ok(())
}

/// Resolve an argument vector (everything after `scriptbox`) to an [`Action`].
fn parse(args: &[String]) -> Result<Action> {
    match args.first().map(String::as_str) {
        None => {
            usage();
            bail!("no script given");
        }
        Some("--version" | "-V") => Ok(Action::Version),
        Some("--help" | "-h") => Ok(Action::Help),
        Some("hash") => Ok(Action::Hash(script_arg(args, "hash")?)),
        Some("pin") => Ok(Action::Pin(script_arg(args, "pin")?)),
        _ => parse_run(args),
    }
}

/// Extract the single script path argument for a `hash`/`pin` subcommand.
fn script_arg(args: &[String], sub: &str) -> Result<PathBuf> {
    match args.get(1) {
        Some(p) => Ok(PathBuf::from(p)),
        None => bail!("`{sub}` needs a script path: `scriptbox {sub} <script>`"),
    }
}

/// Parse the run form: leading scriptbox flags, then the script (the first
/// argument that is an existing file and is not a program binary); anything
/// before it is the interpreter + its flags, anything after it goes to the
/// script.
fn parse_run(args: &[String]) -> Result<Action> {
    let mut rewrite_argv0 = true;
    let mut rest = args;
    while let Some(flag) = rest.first() {
        match flag.as_str() {
            "--argv0-rewrite" => rewrite_argv0 = true,
            "--no-argv0-rewrite" => rewrite_argv0 = false,
            _ => break,
        }
        rest = &rest[1..];
    }

    // Interpreters (`/bin/bash`, a pkgx Mach-O) are ELF/Mach-O; scripts are text.
    // This lets the interpreter be given as a bare name (`bash`) or a full path
    // (`/bin/bash`) without being mistaken for the script, while a bare
    // interpreter name simply isn't a file and is skipped.
    let script_idx = rest
        .iter()
        .position(|a| {
            let p = Path::new(a);
            p.is_file() && !is_program_binary(p)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no script file found in arguments: {rest:?}\n\
                 usage: scriptbox [interpreter] <script> [args...]"
            )
        })?;

    Ok(Action::Run(run::RunSpec {
        interp_override: rest[..script_idx].to_vec(),
        script: PathBuf::from(&rest[script_idx]),
        script_args: rest[script_idx + 1..].to_vec(),
        rewrite_argv0,
    }))
}

/// True if the file begins with an ELF or Mach-O magic number, i.e. it's a
/// compiled program (an interpreter), not a text script.
fn is_program_binary(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(
        magic,
        [0x7f, b'E', b'L', b'F']                       // ELF
            | [0xfe, 0xed, 0xfa, 0xce]                 // Mach-O 32 BE
            | [0xfe, 0xed, 0xfa, 0xcf]                 // Mach-O 64 BE
            | [0xce, 0xfa, 0xed, 0xfe]                 // Mach-O 32 LE
            | [0xcf, 0xfa, 0xed, 0xfe]                 // Mach-O 64 LE
            | [0xca, 0xfe, 0xba, 0xbe]                 // Mach-O fat BE
            | [0xbe, 0xba, 0xfe, 0xca] // Mach-O fat LE
    )
}

fn usage() {
    eprintln!(
        "scriptbox {VERSION} - run a script from an immutable copy\n\
\n\
USAGE:\n\
    scriptbox [FLAGS] [INTERPRETER [IARGS...]] <SCRIPT> [ARGS...]\n\
    scriptbox pin  <SCRIPT>     print a pin-able `# /// scriptbox` block\n\
    scriptbox hash <SCRIPT>     print the script's sha256 pin\n\
\n\
SHEBANG:\n\
    #!/usr/bin/env -S scriptbox bash      interpreter on the shebang line\n\
    #!/usr/bin/env scriptbox              + `# /// scriptbox` frontmatter\n\
\n\
FLAGS:\n\
    --no-argv0-rewrite   keep $0 as the fd path (default: rewrite to the real\n\
                         path for bash/zsh; other shells always see the fd path)\n\
    -V, --version\n\
    -h, --help\n\
\n\
Self-locating scripts should read $SCRIPTBOX_SOURCE (the real path) - e.g.\n\
    SELF=\"${{SCRIPTBOX_SOURCE:-${{BASH_SOURCE[0]}}}}\"\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(name: &str, contents: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "scriptbox-args.{}.{}.{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_and_help() {
        assert!(matches!(
            parse(&argv(&["--version"])).unwrap(),
            Action::Version
        ));
        assert!(matches!(parse(&argv(&["-V"])).unwrap(), Action::Version));
        assert!(matches!(parse(&argv(&["--help"])).unwrap(), Action::Help));
    }

    #[test]
    fn hash_and_pin_need_a_path() {
        assert!(matches!(
            parse(&argv(&["hash", "x.sh"])).unwrap(),
            Action::Hash(_)
        ));
        assert!(matches!(
            parse(&argv(&["pin", "x.sh"])).unwrap(),
            Action::Pin(_)
        ));
        assert!(parse(&argv(&["hash"])).is_err());
        assert!(parse(&argv(&["pin"])).is_err());
    }

    #[test]
    fn no_args_is_an_error() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn bare_interpreter_name_then_script() {
        let s = tmp("run.sh", b"#!/bin/bash\necho hi\n");
        let a = parse(&argv(&["bash", s.to_str().unwrap(), "one", "two"])).unwrap();
        let Action::Run(spec) = a else {
            panic!("expected Run")
        };
        assert_eq!(spec.interp_override, vec!["bash"]);
        assert_eq!(spec.script, s);
        assert_eq!(spec.script_args, vec!["one", "two"]);
        assert!(spec.rewrite_argv0);
        let _ = std::fs::remove_file(&s);
    }

    #[test]
    fn interpreter_given_as_a_binary_path_is_not_the_script() {
        // The current test binary is an ELF/Mach-O; it must be treated as the
        // interpreter, and the text file as the script.
        let exe = std::env::current_exe().unwrap();
        let s = tmp("real.sh", b"#!/bin/bash\necho hi\n");
        let a = parse(&argv(&[exe.to_str().unwrap(), s.to_str().unwrap()])).unwrap();
        let Action::Run(spec) = a else {
            panic!("expected Run")
        };
        assert_eq!(spec.script, s);
        assert_eq!(spec.interp_override, vec![exe.to_str().unwrap()]);
        let _ = std::fs::remove_file(&s);
    }

    #[test]
    fn no_argv0_rewrite_flag_is_consumed() {
        let s = tmp("flag.sh", b"#!/bin/bash\n:\n");
        let a = parse(&argv(&["--no-argv0-rewrite", "bash", s.to_str().unwrap()])).unwrap();
        let Action::Run(spec) = a else {
            panic!("expected Run")
        };
        assert!(!spec.rewrite_argv0);
        assert_eq!(spec.interp_override, vec!["bash"]);
        let _ = std::fs::remove_file(&s);
    }

    #[test]
    fn no_script_file_is_an_error() {
        assert!(parse(&argv(&["bash", "definitely-not-a-real-file.sh"])).is_err());
    }

    #[test]
    fn is_program_binary_distinguishes_binaries_from_scripts() {
        let exe = std::env::current_exe().unwrap();
        assert!(
            is_program_binary(&exe),
            "the test binary should read as a program"
        );
        let text = tmp("text.sh", b"#!/bin/bash\necho hi\n");
        assert!(!is_program_binary(&text));
        let _ = std::fs::remove_file(&text);
        assert!(!is_program_binary(Path::new("/no/such/path")));
    }
}
