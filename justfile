# Development recipes; CI runs the same checks (.github/workflows).

# Run all checks: formatting, linting, tests
check: fmt clippy test

# Check formatting
fmt:
    cargo fmt --all -- --check

# Run the linter over every target, matching CI
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run all tests (unit + end-to-end against the installed shells)
test:
    cargo test --workspace --all-features

# Build the release binary
build:
    cargo build --release

# Prove the Linux memfd path end-to-end in a container (this box is arm64;
# the CI self-hosted runner is Linux, macOS coverage comes from hosted CI).
test-linux:
    docker run --rm -v "$PWD:/src:ro" -e CARGO_TARGET_DIR=/tmp/target rust:1-slim bash -c '\
        apt-get -qq update >/dev/null && apt-get -qq install -y zsh ksh dash >/dev/null && \
        cp -r /src /work && cd /work && cargo test'

# Coverage via cargo-llvm-cov (matches CI)
coverage:
    cargo llvm-cov --workspace --all-features

# Security audit (also runs weekly in CI)
audit:
    cargo audit

# Verify the declared MSRV still builds+tests (slow; run at release time)
msrv:
    cargo msrv verify
