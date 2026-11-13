
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Tidy the code, then see that it works. A prototype runs no clippy; the
# production lints come with its promotion.
check:
    cargo fmt
    cargo test

