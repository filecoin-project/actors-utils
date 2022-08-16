install-toolchain:
	rustup component add rustfmt
	rustup component add clippy
	rustup target add wasm32-unknown-unknown

build: install-toolchain
	cargo build --workspace

check: install-toolchain
	cargo fmt --check
	cargo clippy --workspace -- -D warnings

check-build: check
	cargo build --workspace

# run all tests, this will not work if using RUSTFLAGS="-Zprofile" to generate profile info or coverage reports
# as any WASM targets will fail to build
test: install-toolchain
	cargo test

# tests excluding actors so we can generate coverage reports during CI build
# WASM targets such as actors do not support this so are excluded
test-coverage: install-toolchain
	cargo test --workspace --exclude greeter --exclude fil_integration_tests

# separate actor testing stage to run from CI without coverage support
test-actors: install-toolchain
	cargo test --package greeter --package fil_integration_tests

clean:
	cargo clean
	find . -name '*.profraw' -delete
