# print options
default:
    @just --list --unsorted

# install cargo tools
init:
    cargo upgrade --incompatible
    cargo update

# check code
check:
    cargo check
    cargo fmt --all -- --check
    cargo clippy --all-targets --features "full"

# fix code
fix:
    cargo fmt --all
    cargo clippy --allow-dirty --fix --features "full"

# build project
build:
    cargo build --features "full"

# execute tests
test:
    cargo test --features "full"

# execute benchmarks
bench:
    cargo bench --features "full"

