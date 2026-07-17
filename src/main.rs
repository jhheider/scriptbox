//! scriptbox — read a script fully into an immutable copy at invoke, verify an
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

fn main() {
    if let Err(e) = real_main() {
        eprintln!("scriptbox: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            usage();
            bail!("no script given");
        }
        Some("--version" | "-V") => {
            println!("scriptbox {VERSION}");
            Ok(())
        }
        Some("--help" | "-h") => {
            usage();
            Ok(())
        }
        Some("hash") => pin::hash(&script_arg(&args, "hash")?),
        Some("pin") => pin::pin(&script_arg(&args, "pin")?),
        _ => dispatch_run(&args),
    }
}

/// Extract the single script path argument for a `hash`/`pin` subcommand.
fn script_arg(args: &[String], sub: &str) -> Result<PathBuf> {
    match args.get(1) {
        Some(p) => Ok(PathBuf::from(p)),
        None => bail!("`{sub}` needs a script path: `scriptbox {sub} <script>`"),
    }
}

/// The run path. Parses leading scriptbox flags, then locates the script (the
/// first argument that is an existing file); anything before it is the
/// interpreter + its flags, anything after it is passed to the script.
fn dispatch_run(args: &[String]) -> Result<()> {
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

    // The script is the first argument that is an existing file *and* is not a
    // program binary — interpreters (`/bin/bash`, a pkgx Mach-O) are ELF/Mach-O,
    // scripts are text. This lets the interpreter be given as a bare name
    // (`bash`) or a full path (`/bin/bash`) without being mistaken for the
    // script, while a bare interpreter name simply isn't a file and is skipped.
    let script_idx = rest
        .iter()
        .position(|a| {
            let p = Path::new(a);
            p.is_file() && !is_program_binary(p)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no script file found in arguments: {rest:?}\n\
                 usage: scriptbox [interpreter] <script> [args…]"
            )
        })?;

    let spec = run::RunSpec {
        interp_override: rest[..script_idx].to_vec(),
        script: PathBuf::from(&rest[script_idx]),
        script_args: rest[script_idx + 1..].to_vec(),
        rewrite_argv0,
    };

    // On success `run` never returns (it execs); it only returns `Err`.
    let never = run::run(spec)?;
    match never {}
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
        "scriptbox {VERSION} — run a script from an immutable copy\n\
\n\
USAGE:\n\
    scriptbox [FLAGS] [INTERPRETER [IARGS…]] <SCRIPT> [ARGS…]\n\
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
Self-locating scripts should read $SCRIPTBOX_SOURCE (the real path) — e.g.\n\
    SELF=\"${{SCRIPTBOX_SOURCE:-${{BASH_SOURCE[0]}}}}\"\n"
    );
}
