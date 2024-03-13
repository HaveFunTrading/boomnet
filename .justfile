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
    cargo clippy --all-targets --all-features

# fix code
fix:
    cargo fmt --all
    cargo clippy --all-targets --all-features --allow-dirty --fix

# build project
build:
    cargo build --all-targets

# execute tests
test:
    cargo test


