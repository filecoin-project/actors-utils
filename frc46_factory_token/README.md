# frc46_factory_token

A configurable native FVM actor that can be used as a factory to implement [FRC-0046](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md) tokens, based on [frc46_token](../frc46_token/README.md)

Basic configuration is set at construction time as an immutable part of the token state, allowing many tokens to reuse the same actor code.

This actor also serves as an example of a more complicated token implementation that carries its own state along with the `TokenState` from [frc46_token](../frc46_token/README.md) 

This actor is also used as the token implementation in many of the [integration tests](../fil_token_integration/tests/)

## Construction
The `Constructor` method takes the following params struct which configures the new token:

```Rust
pub struct ConstructorParams {
    pub name: String,
    pub symbol: String,
    pub granularity: u64,
    /// authorised mint operator
    /// only this address can mint tokens or remove themselves to permanently disable minting
    pub minter: Address,
}
```

These params are set once at construction time and cannot be changed for the life of that token instance, with the exception of being able to clear the `minter` address one time to permanently disable minting.

No checks or validation are carried out, the onus is on the user to provide appropriate values for their token.

## Minting 
A basic minting strategy is used, with a single address nominated at construction time as the authorised minter and no limit enforced on the amount they can mint.

Calls to `Mint` from any other address will abort.

Minting can be permanently disabled by calling the `DisableMint` method from the authorised minter address. This clears the stored minter address and any further calls to either `Mint` or `DisableMint` will immediately abort.


## token_impl
The core of the factory token implementation lives inside the [token_impl](./token_impl/) crate, so it can be imported without potential conflicts arising from the un-mangled `invoke` method found in the actor code.