# FIL Token Actor

This creates a FIP-??? compliant fungible token that wraps the value of of
native FIL.

Calling the `mint` (non-standard) method on this actor will increase the
caller's WFIL balance by the amount of FIL sent in the message. The WFIL can be
transferred between any FIP-??? compliant wallets (which have `token_received`
hook) by either direct transfer or delegated transfer.

https://etherscan.io/tx/0x96a7155b44b77c173e7c534ae1ceca536ba2ce534012ff844cf8c1737bc54921

## Direct transfer flow

Call `transfer(TransferParams)` to directly transfer tokens from the caller's
wallet to a receiving address.

## Delegated transfer flow

1. Call `increase_allowance(AllowanceParams)` to increase the "spenders"
   allowance
2. The "spender" can then call this actor with
   `transfer_from(TransferFromParams)` to

## Transferring tokens to this actor

Transferring WFIL to the address of this actor itself will result in the WFIL
being burnt and the corresponding FIL being credited back. This prevents the
case where tokens are unintentionally locked in to contracts that are unable to
receive them. This flow requires the actor to implement its own `token_received`
hook.

However, also compliant with the token standard would be for this actor to omit
the `token_received` hook. In this case, transfers to the contract itself would
simply be rejected, which also prevents unintentional loss of tokens.
