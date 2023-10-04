# Filecoin

[Filecoin](https://filecoin.io) is a decentralized storage network designed to
store humanity's most important information.

This repo contains utilities and libraries to work with the
[Filecoin Virtual Machine](https://fvm.filecoin.io/)

[![Coverage Status](https://coveralls.io/repos/github/helix-onchain/filecoin/badge.svg?branch=main)](https://coveralls.io/github/helix-onchain/filecoin?branch=main)

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

## License

Dual-licensed: [MIT](./LICENSE-MIT),
[Apache Software License v2](./LICENSE-APACHE).

## Testing

For local coverage testing, please install the `grcov` crate.

<sub>Copyright Protocol Labs, Inc, 2022</sub>
