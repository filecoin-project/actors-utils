# frc53_nft

This acts as the reference library for FRC53. While remaining complaint with the
spec, this library is opinionated in its batching, minting and storage
strategies to optimize for common usage patterns.

For example, write operations are generally optimised over read operations as
on-chain state can be read by direct inspection (rather than via an actor call)
in many cases.
