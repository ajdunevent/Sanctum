#!/bin/sh
# Vendors crates into the dist tarball so release builds can run offline.
export DIST="$1"
export SOURCE_ROOT="$2"

cd "$SOURCE_ROOT"
mkdir "$DIST"/.cargo
cargo vendor | sed 's/^directory = ".*"/directory = "vendor"/g' > "$DIST"/.cargo/config.toml
mv vendor "$DIST"
