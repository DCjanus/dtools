install: prepare
    cargo install --path . -f --locked

prepare:
    just fix
    just fmt
    just check

fmt:
    cargo sort
    cargo +nightly fmt

fix:
    cargo +nightly clippy --fix --allow-dirty

check:
    cargo machete
    cargo +nightly fmt -- --check
    cargo +nightly clippy -- -D warnings
