
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# A prototype runs no clippy; the production lints come with its promotion.
# Format the code, then run the tests.
check:
    cargo fmt
    cargo test

# Hooks pick up the new binary on their next call. Adding the hooks to Claude
# Code's settings is a separate, one-time step: see `hookwatch install`.
# Install (or reinstall) the hookwatch command.
install:
    cargo install --locked --path .

# `hookwatch put-jigs` checks the file loads, backs up the old one, and needs
# a terminal, so an agent's Bash cannot run it (it changes which jigs judge
# the agent). Built from this checkout, not the installed binary.
# Put jigs/jigs.json in effect (human only).
jigs:
    cargo run --quiet -- put-jigs jigs/jigs.json

