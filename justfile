# Default recipe
_default:
   just --list

# ======================== #
# Aliases                  #
# ======================== #

alias b := build
alias c := check
alias d := doc
alias f := fmt
alias m := msrv
alias t := test
alias p := pre-push

# ======================== #
# Toolchains               #
# ======================== #

# Nightly toolchain
nightly := 'nightly'

# Stable toolchain (same as rust-version)
stable := '1.93.0'

# MSRV toolchain
msrv := '1.63.0'

# ======================== #
# Recipes                  #
# ======================== #

# Build the project
build:
   cargo +{{stable}} build --all-targets --all-features

# Check MSRV
msrv:
   ./ci/pin-msrv.sh
   cargo +{{msrv}} build --workspace --lib --tests --no-default-features --features miniscript/no-std,bdk_chain/hashbrown
   cargo +{{msrv}} build --workspace --lib --tests --all-features

# Format all code
fmt:
   cargo +{{nightly}} fmt

# Check code: formatting, compilation, linting, and commit signature
check:
   just _verify-head
   cargo +{{nightly}} fmt --all -- --check
   cargo +{{stable}} check --no-default-features --features miniscript/no-std,bdk_chain/hashbrown
   cargo +{{stable}} check --features default
   cargo +{{stable}} check --all-targets --all-features
   cargo +{{stable}} clippy --all-features --all-targets -- -D warnings

# Run all tests on the workspace with all features
test:
   cargo +{{stable}} test --all-features

# Run doctests. Build and check docs
doc:
   cargo +{{stable}} test --doc --all-features
   RUSTDOCFLAGS='-D warnings' cargo +{{stable}} doc --workspace --all-features --no-deps

# Run pre-push suite: format, check, and test
pre-push: fmt check test doc

# Git verify the HEAD commit
_verify-head:
   @[ "$(git log --pretty='format:%G?' -1 HEAD)" = "N" ] && \
       echo "\n⚠️  Unsigned commit: BDK requires that commits be signed." || \
       true
