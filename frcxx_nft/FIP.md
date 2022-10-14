# Non-Fungible Token Standard

## Simple Summary

A standard interface for native actor non-fungible tokens (NFTs).

## Abstract

This proposal provides a standard API for the implementation of non-fungible
tokens (NFTs) as FVM native actors. The proposal learns from NFT standards
developed for other blockchain ecosystems, being heavily inspired by
[ERC-721](https://eips.ethereum.org/EIPS/eip-721). However, as a design goal,
this proposal aims to complement the existing fungible token interface described
in [FRC-0046](https://github.com/filecoin-project/FIPs/pull/435/files). As such
it brings along equivalent specifications for:

- Standard token/name/symbol/supply/balance queries
- Standard allowance protocol for delegated control, but with an API robust to
  [front-running](https://ieeexplore.ieee.org/document/8802438)
- Mandatory universal receiver hooks for incoming tokens

The interface has been designed with gas-optimisations in mind and hence methods
support batch-operations where practical.

## Change Motivation

The concept of a non-fungible token is widely established in other blockchains.
As on other blockchains, a complementary NFT standard to FRC-0046 allows the
ownership of uniquely identifiable assets to be tracked on-chain. The Filecoin
ecosystem will benefit from a standard API implemented by these actors. A
standard permits easy building of UIs, wallets, tools, and higher level
applications on top of a variety of tokens representing different assets.

## Specification

Methods and types are described with a Rust-like pseudocode. All parameters and
return types are IPLD types in a CBOR tuple encoding.

Methods are to be dispatched according to the calling convention described in
[FRC-0042](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0042.md).

### Interface

An actor implementing a FRC-00XX token must provide the following methods.

```rust
type TokenID = u64;

/// A descriptive name for the collection of NFTs in this actor
fn Name() -> String

/// An abbreviated (ticker) name for the NFT collection
fn Symbol() -> String

/// Returns the total number of tokens in this collection
/// Must be non-negative
/// Must equal the balance of all adresses
/// Should equal the sum of all minted tokens less tokens burnt
fn TotalSupply() -> u64

/// Returns a link that resolves to the metadata for a particular token
fn MetadataID(token_id: TokenID) -> String

/// Returns the balance of an address which is the number of unique NFTs held
/// Must be non-negative
fn Balance(owner: Address) -> String

/// Transfers tokens from caller to the specified address
/// For each token being transferred, the caller must:
/// - Be the owner OR
/// - Be an approved operator on the token OR
/// - Be an approved operator on the owner
/// The entire batch of transfers aborts if any of the specified tokens do not meet at least one of the above criteria
/// Transferring to the caller must be treated as a normal transfer
/// Returns the resulting balances for the from and to addresses
/// The operatorData is passed through to the receiver hook directly
/// Aborts if the receiver hook on the `to` address aborts
fn Transfer({to: Address, tokenIDs: TokenID[], operatorData: Bytes})
  -> {fromBalance: u64, toBalance: u64}

/// Approves an address as the specified operator for a set of tokens
/// The entire batch of approvals aborts if any of the tokens are not owned by the caller
fn Approve({operator: Address, tokenIDs: TokenID[]}) -> ()

/// Revokes an address as the specified operator for a set of tokens
/// Tokens that are not owned by the caller are ignored
fn RevokeApproval({operator: Address, tokenIDs: TokenID[]}) -> ()

/// Returns whether an address is an approved operator for a particular set of tokens
/// The owner of a token is implicitly considered as a valid operator of that token
fn IsApprovedFor({operator: Address, tokenIDs: TokenID[]}) -> bool[]

/// Approves the specified address as an approved operator for any token (including future tokens) that are owned by the caller's address
fn ApproveForAll({operator: Address}) -> ()

/// Returns whether an address is an approved operator for another address
/// Every address is implicitly considered a valid operator of itself
fn IsApprovedForAll({operator: Address, owner: Address}) -> bool[]

/// Revokes an address as a specifed operator for the calling account
fn RevokeApprovalForAll({operator: Address}) -> ()
```

### Receiver Interface

An actor must implement a receiver hook to receive NFTs. The receiver hook is
defined in FRC-0046 and must not abort when handling an incoming token. When
transferring batch of tokens, the receiver hook is invoked once meaning the
entire set of NFTs is either accepted or rejected (by aborting).

```rust
/// Type of the payload accompanying the receiver hook for a FRCXX NFT.
struct FRCXXTokensReceived {
    // The tokens being transferred
    tokenIDs: TokenID[],
    // The address to which tokens were credited (which will match the hook receiver)
    to: Address,
    // The actor which initiated the mint or transfer
    operator: Address,
    // Arbitrary data provided by the operator when initiating the transfer
    operatorData: Bytes,
    // Arbitrary data provided by the token actor
    tokenData: Bytes,
}

/// Receiver hook type value for an FRC-00XX token
const FRCXXTokenType = frc42_hash("FRCXX")

// Invoked by a NFT actor after transfer of tokens to the receiver’s address.
// The NFT collection state must be persisted such that the receiver will observe the new balances.
// To reject the transfer, this hook must abort.
fn Receive({type: uint32, payload: []byte})
```

### Behaviour

**Universal receiver hook**

The NFT collection must invoke the receiver hook method on the receiving address
whenever it credits tokens. The `type` parameter must be `FRCXXTokenType` and
the payload must be the IPLD-CBOR serialized `FRCXXTokensReceived` structure.

The attempted credit is only persisted if the receiver hook is implemented and
does not abort. A mint or transfer operation should abort if the receiver hook
does, or in any case must not credit tokens to that address.

**Minting**

API methods for minting are left unspecified. A newly minted token cannot have
the same ID as an existing token or a token that was previously burned. Minting
must invoke the receiver hook on the receiving address and fail if it aborts.

**Transfers**

Empty transfers are allowed, including when the `from` address has zero balance.
An empty transfer must invoke the receiver hook of the `to` address and abort if
the hook aborts. An empty transfer can thus be used to send messages between
actors in the context of a specific NFT collection.

**Operators**

Operators can be approved at two separate levels.

_Token level operators_ are approved by the owner of the token via the `Approve`
method. If an account is an operator on a token, it is permitted to debit (burn
or transfer) that token. An NFT can have many operators at a time, but
transferring the token will revoke approval on all its existing operators.

_Account level operators_ are approved via the `ApproveForAll` method. If an
owner approves an operator at the account level, that operator has permission to
debit any token belonging to the owner's account. This includes tokens that are
not yet owned by the account at the time of approval.

**Addresses**

Addresses for receivers and operators must be resolvable to an actor ID.
Balances must only be credited to an actor ID. All token methods must attempt to
resolve addresses provided in parameters to actor IDs. A token should attempt to
initialise an account for any address which cannot be resolved by sending a
zero-value transfer of the native token to the address.

Note that this means that an uninitialized actor-type (f2) address cannot
receive tokens or be authorized as an operator. Future changes to the FVM may
permit initialization of such addresses by sending a message to them, in which
case they should automatically become functional for this standard.

**Extensions**

An NFT collection may implement other methods for transferring tokens and
managing operators. These must maintain the invariants about supply and
balances, and invoke the receiver hook when crediting tokens.

An NFT collection may implement restrictions on allowances and transfer of
tokens.

## Design Rationale

### Synergy with fungible tokens

In order for higher synergy with the existing FRC-0046 fungible token standard,
this proposal aims for a conceptually similar interface in terms of balances,
supply and operators. For the same safety reasons described in FRC-0046, this
token standard requires a universal receiver on actors that wish to hold tokens.

This allows easier interactions between fungible tokens and NFTs and opens
possibilities for composition of the the two standards to represent different
structures of ownership such as fractionalized NFTs or semi-fungible tokens.
Instead of encoding such semantics directly into the standard, a minimal
interface is proposed instead to make the primary simple use cases more
efficient and straightforward.

### Transfers

There is no separate method for transfers by owners v.s. transfers by operators.
The transfer method is only given the list of token ids that the caller wishes
to transfer and for each token asserts that:

- The caller is the owner of the token OR
- The caller is an approved operator on the token OR
- The caller is an approved operator on the account that owns the token

## Backwards Compatability

There are no implementatons of NFT collections yet on Filecoin.

## Test Cases

Extensive test cases are present in the implementation of this proposal at
https://github.com/helix-onchain/filecoin/tree/main/frcxx_nft.

## Security Considerations

### Reentrancy

Receiver hooks introduce the possibility of complex call flows, the most
concerning of which might be a malicious receiver that calls back to a token
actor in an attempt to exploit a re-entrancy bug. We expect that one or more
high quality reference implementations of the token state and logic will keep
the vast majority of NFT actors safe. We judge the risk to more complex actors
as lesser than the aggregate risk of losses due to misdirected transfers.

## Incentive Considerations

N/A

## Product Considerations

??

## Implementation

An implementation of this standard is in development at
https://github.com/helix-onchain/filecoin/tree/main/frcxx_nft.

## Copyright

Copyright and related rights waived via
[CC0](https://creativecommons.org/publicdomain/zero/1.0/).
