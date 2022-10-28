use cid::Cid;
use frc42_dispatch::method_hash;
use frcxx_nft::state::TokenID;
use fvm::{executor::ApplyRet, externs::Externs};
use fvm_integration_tests::tester::Tester;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::{address::Address, ActorID};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

use super::TestHelpers;

// this is here so we don't need to link every test against the basic_token_actor
// otherwise we can't link against frc46_test_actor or any other test/example actors,
// because the invoke() functions will conflict at link time
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    pub initial_owner: Address,
    pub metadata: Vec<Cid>,
    pub operator_data: RawBytes,
}

impl Cbor for MintParams {}

pub trait NFTHelpers {
    /// Get balance from token actor for a given address
    /// This is a very common thing to check during tests
    fn nft_balance(&mut self, operator: Address, token_actor: Address, target: Address) -> u64;

    /// Mint tokens from token_actor to target address
    fn mint_nfts(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: usize,
        operator_data: RawBytes,
    ) -> ApplyRet;

    /// Mint tokens from token_actor to target address and assert a successful result
    fn mint_nfts_ok(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: usize,
        operator_data: RawBytes,
    ) -> ApplyRet;

    /// Check token balance, asserting that balance matches the provided amount
    fn assert_nft_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: u64,
    );

    /// Check token balance, asserting a zero balance
    fn assert_nft_balance_zero(&mut self, operator: Address, token_actor: Address, target: Address);

    /// Check the total supply, asserting that it matches the provided amount
    fn assert_nft_total_supply(&mut self, operator: Address, token_actor: Address, amount: u64);

    /// Check the total supply, asserting that it is zero
    fn assert_nft_total_supply_zero(&mut self, operator: Address, token_actor: Address);

    /// Check the tokens owner, asserting that it is owned by the specified ActorID
    fn assert_nft_owner(
        &mut self,
        operator: Address,
        token_actor: Address,
        token_id: TokenID,
        owner: ActorID,
    );
}

impl<BS: Blockstore, E: Externs> NFTHelpers for Tester<BS, E> {
    fn nft_balance(&mut self, operator: Address, token_actor: Address, target: Address) -> u64 {
        let params = RawBytes::serialize(target).unwrap();
        let ret_val =
            self.call_method(operator, token_actor, method_hash!("BalanceOf"), Some(params));
        ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap()
    }

    fn mint_nfts(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: usize,
        operator_data: RawBytes,
    ) -> ApplyRet {
        let mint_params = MintParams {
            initial_owner: target,
            metadata: vec![Cid::default(); amount],
            operator_data,
        };
        let params = RawBytes::serialize(mint_params).unwrap();
        self.call_method(operator, token_actor, method_hash!("Mint"), Some(params))
    }

    fn mint_nfts_ok(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: usize,
        operator_data: RawBytes,
    ) -> ApplyRet {
        let ret = self.mint_nfts(operator, token_actor, target, amount, operator_data);
        assert!(ret.msg_receipt.exit_code.is_success());
        ret
    }

    fn assert_nft_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: u64,
    ) {
        let balance = self.nft_balance(operator, token_actor, target);
        assert_eq!(balance, amount);
    }

    fn assert_nft_balance_zero(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) {
        let balance = self.nft_balance(operator, token_actor, target);
        assert_eq!(balance, 0);
    }

    fn assert_nft_total_supply(&mut self, operator: Address, token_actor: Address, amount: u64) {
        let ret_val = self.call_method(operator, token_actor, method_hash!("TotalSupply"), None);
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        assert_eq!(total_supply, amount);
    }

    fn assert_nft_total_supply_zero(&mut self, operator: Address, token_actor: Address) {
        self.assert_nft_total_supply(operator, token_actor, 0);
    }

    fn assert_nft_owner(
        &mut self,
        operator: Address,
        token_actor: Address,
        token_id: TokenID,
        actor: ActorID,
    ) {
        let params = RawBytes::serialize(token_id).unwrap();
        let ret_val =
            self.call_method(operator, token_actor, method_hash!("OwnerOf"), Some(params));
        let owner = ret_val.msg_receipt.return_data.deserialize::<ActorID>().unwrap();
        assert_eq!(owner, actor);
    }
}
