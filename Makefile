build:
	cargo build --workspace

check-build: check
	cargo build --workspace --all-targets --all-features

check:
	cargo fmt --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test

clean:
	cargo clean