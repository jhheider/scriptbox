# scriptbox real-world compatibility suite: reproducible cross-shell harness.
#
# Build + run (from the repo root):
#     docker build -t scriptbox-compat -f compat/Dockerfile .
#     docker run --rm scriptbox-compat
#
# What this DOES cover: the Linux frozen-copy path: a sealed `memfd`
# (F_SEAL_WRITE), exec'd as /proc/self/fd/N, across all four shells scriptbox
# supports.
#
# What it does NOT cover: macOS's frozen-copy path is different (a written-then-
# unlinked private temp file, exec'd as /dev/fd/N). Docker is Linux, so that path
# cannot be exercised here; run `sh compat/run.sh <scriptbox>` on real macOS
# separately for it. (fish/nushell are intentionally out of scope; scriptbox does
# not wrap them.)

FROM rust:1-slim-bookworm

# The four shells scriptbox supports. None but bash are guaranteed on a slim
# image, and each needs its own package:
#   bash: the common case; BASH_ARGV0 exists on >=5 so `$0` rewrite works in-run
#   zsh:  `$0` rewrite uses `0=`; source-mode dot-sourcing keeps the fd path
#   dash: Debian's /bin/sh, but the `dash` *binary* needs the package; no in-run
#          `$0` rewrite mechanism, so it needs `--argv0 source`
#   ksh:  ksh93; like dash, needs `--argv0 source` for a real `$0`
# curl is for the read-only pass over real public installers; shellcheck for the
# "scriptbox adds no linter findings" check (original vs `scriptbox emit`).
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash zsh dash ksh curl ca-certificates shellcheck \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /scriptbox

# Build scriptbox first (cached unless sources change), then copy the harness.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# --features subscripts pulls the shell parser (matches the release binaries), so
# the suite can exercise `--subscripts`. Drop it to test the lean default build.
RUN cargo build --release --features subscripts

COPY compat ./compat

CMD ["sh", "compat/run.sh", "target/release/scriptbox"]
