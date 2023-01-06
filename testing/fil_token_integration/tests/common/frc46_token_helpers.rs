use frc42_dispatch::method_hash;
use fvm::{executor::ApplyRet, externs::Externs};
use fvm_integration_tests::tester::Tester;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

use super::TestHelpers;

// this is here so we don't need to link every test against the basic_token_actor
// otherwise we can't link against frc46_test_actor or any other test/example actors,
// because the invoke() functions will conflict at link time
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct MintParams {
    pub initial_owner: Address,
    pub amount: TokenAmount,
    pub operator_data: RawBytes,
}

/// Helper routines to simplify common token operations
pub trait TokenHelper {
    /// Get balance from token actor for a given address
    /// This is a very common thing to check during tests
    fn token_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount;

    /// Mint tokens from token_actor to target address
    fn mint_tokens(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
        operator_data: RawBytes,
    ) -> ApplyRet;

    /// Mint tokens from token_actor to target address and assert a successful result
    fn mint_tokens_ok(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
        operator_data: RawBytes,
    ) -> ApplyRet;

    /// Check token balance, asserting that balance matches the provided amount
    fn assert_token_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
    );

    /// Check token balance, asserting a zero balance
    fn assert_token_balance_zero(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    );
}

impl<B: Blockstore, E: Externs> TokenHelper for Tester<B, E> {
    fn token_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount {
        let params = RawBytes::serialize(target).unwrap();
        let ret_val =
            self.call_method(operator, token_actor, method_hash!("BalanceOf"), Some(params));
        ret_val.msg_receipt.return_data.deserialize::<TokenAmount>().unwrap()
    }

    fn mint_tokens(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
        operator_data: RawBytes,
    ) -> ApplyRet {
        let mint_params = MintParams { initial_owner: target, amount, operator_data };
        let params = RawBytes::serialize(mint_params).unwrap();
        self.call_method(operator, token_actor, method_hash!("Mint"), Some(params))
    }

    fn mint_tokens_ok(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
        operator_data: RawBytes,
    ) -> ApplyRet {
        let ret = self.mint_tokens(operator, token_actor, target, amount, operator_data);
        assert!(ret.msg_receipt.exit_code.is_success(), "{ret:?}");
        ret
    }

    fn assert_token_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
    ) {
        let balance = self.token_balance(operator, token_actor, target);
        assert_eq!(balance, amount);
    }

    fn assert_token_balance_zero(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) {
        let balance = self.token_balance(operator, token_actor, target);
        assert_eq!(balance, TokenAmount::zero());
    }
}
