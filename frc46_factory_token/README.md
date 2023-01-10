# frc46_factory_token

A configurable actor that can be used as a factory to implement [FRC-0046](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0046.md) tokens, based on `frc46_token`.

Basic configuration is set at construction time as an immutable part of the token state, allowing many tokens to reuse the same actor code.