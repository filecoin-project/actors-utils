# Filecoin

[Filecoin](https://filecoin.io) is a decentralized storage network designed to
store humanity's most important information.

This repo contains utilities and libraries to work with the
[Filecoin Virtual Machine](https://fvm.filecoin.io/)

[![codecov](https://codecov.io/gh/filecoin-project/actors-utils/graph/badge.svg?token=5I8ddKxkjm)](https://codecov.io/gh/filecoin-project/actors-utils)

## Packages

### fvm_actor_utils

A set of utilities to help write testable native actors for the Filecoin Virtual
Machine. Provides abstractions on top of FVM-SDK functionality that can be
shimmed or mocked in unit tests. This includes helpers for:

- Universal receiver hooks (as defined in
  [FRC-0046](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md))
- IPLD-compatible blockstore
- Messaging and address resolution

### frc42_dispatch

Reference library containing macros for standard method dispatch. A set of CLI
utilities to generate method numbers is also available:
[fvm_dispatch_tools](./fvm_dispatch_tools/)

| Specification                                                                     | Reference Implementation                     | Examples                                         |
| --------------------------------------------------------------------------------- | -------------------------------------------- | ------------------------------------------------ |
| [FRC-0042](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0042.md) | [frc42_dispatch](./frc42_dispatch/README.md) | [greeter](./dispatch_examples/greeter/README.md) |

### frc46_token

Reference library for implementing a standard fungible token in native actors

| Specification                                                                     | Reference Implementation               | Examples                                                                                                                                               |
| --------------------------------------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [FRC-0046](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md) | [frc46_token](./frc46_token/README.md) | [basic_token](./testing/test_actors/actors/basic_token_actor/README.md) [basic_receiver](./testing/test_actors/actors/basic_receiving_actor/README.md) |

### frc53_nft

Reference library for implementing a standard non-fungible token in native
actors

| Specification                                                                     | Reference Implementation           | Examples                                                                                                                                           |
| --------------------------------------------------------------------------------- | ---------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| [FRC-0053](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0053.md) | [frc53_nft](./frc53_nft/README.md) | [basic_nft](./testing/test_actors/actors/basic_nft_actor/README.md) [basic_receiver](./testing/test_actors/actors/basic_receiving_actor/README.md) |

### frc46_factory_token

A configurable actor that can be used as a factory to create instances of
[FRC-0046](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md)-compatible
tokens, based on [frc46_token](./frc46_token/README.md) and implemented
[here](./testing/test_actors/actors/frc46_factory_token/)

## Release Process

This section documents the release process for actor-utils packages.

### Published Packages

The following packages are published to [crates.io](https://crates.io):

- **`fvm_actor_utils`** - Core utilities for FVM native actors
- **`frc42_dispatch`** - Method dispatch macros and utilities  
- **`frc42_hasher`** - FRC-0042 method hashing utilities
- **`frc42_macros`** - FRC-0042 procedural macros
- **`frc46_token`** - Fungible token reference implementation
- **`frc53_nft`** - Non-fungible token reference implementation
- **`fvm_dispatch_tools`** - CLI utilities for method dispatch

#### Release Steps

1. **Version Bumping**
   ```bash
   # Update version in package Cargo.toml
   # Update workspace dependencies in root Cargo.toml if needed
   ```

2. **Pre-publish Validation**
   ```bash
   # Check compilation
   cargo check -p <package-name>
   
   # Dry run publish
   cargo publish --dry-run -p <package-name>
   ```

3. **Publishing**
   ```bash
   # Authenticate with crates.io
   cargo login <your-token>
   
   # Publish in dependency order:
   # 1. frc42_hasher (base dependency)
   # 2. frc42_macros (depends on frc42_hasher)
   # 3. frc42_dispatch (depends on frc42_hasher and frc42_macros)
   # 4. fvm_actor_utils (depends on frc42_dispatch)
   # 5. frc46_token (depends on frc42_dispatch and fvm_actor_utils)
   # 6. frc53_nft (depends on frc42_dispatch and fvm_actor_utils)
   # 7. fvm_dispatch_tools (depends on frc42_dispatch)
   cargo publish -p <package-name>
   ```

4. **Post-Release**
   ```bash
   # Tag the release
   git tag <package-name>@<version>
   git push origin <package-name>@<version>
   ```

#### Coordination Points

- **FVM Releases**: TODO - Document coordination process with FVM releases
- **Built-in Actors / Network Upgrade**: TODO - Document alignment with built-in actors and Network Upgrades

## License

Dual-licensed: [MIT](./LICENSE-MIT),
[Apache Software License v2](./LICENSE-APACHE).

## Testing

The tests require downloading a builtin-actors bundle. Either run the tests with `make test` or run `make test-deps` before running tests manually with cargo.

You can change the actors version by changing `ACTORS_VERSION` in the `Makefile`. If you want to test with a custom bundle entirely, replace the `testing/bundles/builtin-actors.car` symlink with the custom bundle. Note, however, that running `make test` will revert that change, so you'll want to test with `cargo test` manually.

For local coverage testing, please install the `grcov` crate.

<sub>Copyright Protocol Labs, Inc, 2022</sub>
