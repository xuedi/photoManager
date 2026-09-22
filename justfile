app := "photomanager"
dev := "--features devtools"
session := "photomanager-ui"
library := env("PHOTOMANAGER_LIBRARY", home_dir() / "Nextcloud/Photos")

_default:
    @just --list --unsorted

# build a debug binary with the development actions
build:
    cargo build {{dev}}

# run it against the photo library
run: build
    ./target/debug/{{app}}

# build an optimised binary
release:
    cargo build --release

# format, lint and test everything
check:
    cargo fmt --check
    cargo clippy --workspace --all-targets {{dev}} -- -D warnings
    cargo test --workspace {{dev}}

# format the code and apply what clippy can fix
fix:
    cargo fmt
    cargo clippy --workspace --all-targets {{dev}} --fix --allow-dirty -- -D warnings

# unit tests, without the ones that need a display
test:
    cargo test --workspace {{dev}}

# every test, inside a private headless session
test-ui: build
    #!/usr/bin/env bash
    set -euo pipefail
    pinchy --session {{session}} up
    trap 'pinchy --session {{session}} down' EXIT
    PHOTOMANAGER_UI_TESTS=1 pinchy --session {{session}} run -- \
        cargo test --workspace {{dev}} -- --test-threads=1

# click the real binary through a private headless session
smoke: build
    cargo test -p {{app}} {{dev}} --test headless -- --ignored --nocapture

# start a session with the app in it, to look at it
ui dir=library: build
    #!/usr/bin/env bash
    set -euo pipefail
    pinchy --session {{session}} up
    PHOTOMANAGER_LIBRARY={{dir}} pinchy --session {{session}} launch -- ./target/debug/{{app}}
    pinchy --session {{session}} tree

# a screenshot of that session
ui-shot file="/tmp/photomanager.png":
    pinchy --session {{session}} shot {{file}}

# stop the session
ui-down:
    pinchy --session {{session}} down
