// constants for wasm build artifacts
#![allow(dead_code)]
macro_rules! wasm_bin {
    ($x: expr) => {
        concat!(env!("OUT_DIR"), "/bundle/wasm32-unknown-unknown/wasm/", $x, ".wasm")
    };
}

pub const BASIC_NFT_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("basic_nft_actor"));
pub const BASIC_RECEIVING_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("basic_receiving_actor"));
pub const BASIC_TOKEN_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("basic_token_actor"));
pub const BASIC_TRANSFER_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("basic_transfer_actor"));
pub const FRC46_TEST_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("frc46_test_actor"));
pub const FRC53_TEST_ACTOR_BINARY: &[u8] = include_bytes!(wasm_bin!("frc53_test_actor"));
