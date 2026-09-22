app := "photomanager"
dev := "--features devtools"

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

# unit and widget tests
test:
    cargo test --workspace {{dev}}
