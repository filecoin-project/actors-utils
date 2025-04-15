WASM_EXCLUSION = \
	--exclude greeter \
	--exclude helix_integration_tests \
	--exclude basic_token_actor \
	--exclude basic_receiving_actor \
	--exclude basic_nft_actor \
	--exclude basic_transfer_actor \
	--exclude frc46_test_actor \
	--exclude frc46_factory_token \
	--exclude frc53_test_actor

ACTORS_VERSION=v15.0.0
ACTORS_NETWORK=mainnet
ACTORS_BUNDLE_NAME=builtin-actors-${ACTORS_VERSION}-${ACTORS_NETWORK}.car
ACTORS_URL=https://github.com/filecoin-project/builtin-actors/releases/download/${ACTORS_VERSION}/builtin-actors-${ACTORS_NETWORK}.car

# actors are built to WASM via the helix_test_actors crate and be built individually as standalone
# crates so we exclude the from this convenience target
build: install-toolchain
	cargo build --workspace $(WASM_EXCLUSION)

check: install-toolchain fetch-bundle
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings

check-build: check build


testing/bundles/${ACTORS_BUNDLE_NAME}:
	curl -fL --create-dirs --output $@ ${ACTORS_URL}

fetch-bundle: testing/bundles/${ACTORS_BUNDLE_NAME}
	ln -sf ${ACTORS_BUNDLE_NAME} testing/bundles/builtin-actors.car
.PHONY: fetch-bundle


test-deps: install-toolchain fetch-bundle
.PHONY: test-deps

# run all tests, this will not work if using RUSTFLAGS="-Zprofile" to generate profile info or coverage reports
# as any WASM targets will fail to build
test: test-deps
	cargo test

# tests excluding actors so we can generate coverage reports during CI build
# WASM targets such as actors do not support this so are excluded
test-coverage: test-deps
	CARGO_INCREMENTAL=0 \
	RUSTFLAGS='-Cinstrument-coverage -C codegen-units=1 -C llvm-args=--inline-threshold=0 -C overflow-checks=off' \
	LLVM_PROFILE_FILE='target/coverage/raw/cargo-test-%p-%m.profraw' \
	cargo test --workspace $(WASM_EXCLUSION)
	grcov . --binary-path ./target/debug/deps/ -s . -t html --branch --ignore-not-existing --ignore '../*' --ignore "/*" -o target/coverage/html

# Just run the tests we want coverage of (for CI)
ci-test-coverage: test-deps
	rustup component add llvm-tools-preview
	cargo llvm-cov --lcov --output-path ci-coverage.info --workspace $(WASM_EXCLUSION)

# separate actor testing stage to run from CI without coverage support
test-actors: test-deps
	cargo test --package greeter --package helix_integration_tests

install-toolchain:
	rustup show active-toolchain || rustup toolchain install

clean:
	cargo clean
	find . -name '*.profraw' -delete
	rm Cargo.lock
	rm -r testing/bundles

# generate local coverage report in html format using grcov
# install it with `cargo install grcov`
local-coverage: test-coveuage
