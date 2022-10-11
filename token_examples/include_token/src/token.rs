use cid::Cid;
use frc42_dispatch::match_method;
use frc46_token::token::{
    types::{
        AllowanceReturn, BalanceReturn, BurnFromReturn, BurnParams, BurnReturn,
        DecreaseAllowanceParams, FRC46Token, GetAllowanceParams, GranularityReturn,
        IncreaseAllowanceParams, MintReturn, RevokeAllowanceParams, TotalSupplyReturn,
        TransferFromParams, TransferFromReturn, TransferParams, TransferReturn,
    },
    Token, TokenError,
};
use fvm_actor_utils::{
    blockstore::Blockstore, messaging::FvmMessenger, receiver::ReceiverHookError,
};
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    Cbor, RawBytes, DAG_CBOR,
};
use fvm_sdk::{self as sdk, sys::ErrorNumber, NO_DATA_BLOCK_ID};
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use serde::{de::DeserializeOwned, ser::Serialize};
use thiserror::Error;

/// Errors that can occur during the execution of this actor
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error from the underlying token library
    #[error("error in token: {0}")]
    Token(#[from] TokenError),
    /// Error from the underlying universal receiver hook library
    #[error("error calling receiver hook: {0}")]
    Receiver(#[from] ReceiverHookError),
    /// Error from serialising data to RawBytes
    #[error("ipld encoding error: {0}")]
    Encoding(#[from] fvm_ipld_encoding::Error),
}

pub fn caller_address() -> Address {
    let caller = sdk::message::caller();
    Address::new_id(caller)
}

// NOTE: this is mostly lifted from the basic token actor we use in integration testing
// differences are how state load/store is handled
// and the build-in invoke() designed to be called from an upstream invoke()

// this token implementation is designed to be embedded into some larger actor
// where the state contains more than just the token

struct BasicToken<'state> {
    /// Default token helper impl
    util: Token<'state, Blockstore, FvmMessenger>,
    /// basic token identifier stuff, should it go here or store separately alongside the state
    name: String,
    symbol: String,
    granularity: u64,
}

/// Implementation of the token API in a FVM actor
///
/// Here the Ipld parameter structs are marshalled and passed to the underlying library functions
impl FRC46Token<RuntimeError> for BasicToken<'_> {
    fn name(&self) -> String {
        //String::from("FRC-0046 Token")
        self.name.clone()
    }

    fn symbol(&self) -> String {
        //String::from("FRC46")
        self.symbol.clone()
    }

    fn granularity(&self) -> GranularityReturn {
        //1
        self.granularity
    }

    fn total_supply(&self) -> TotalSupplyReturn {
        self.util.total_supply()
    }

    fn balance_of(&self, params: Address) -> Result<BalanceReturn, RuntimeError> {
        Ok(self.util.balance_of(&params)?)
    }

    fn transfer(&mut self, params: TransferParams) -> Result<TransferReturn, RuntimeError> {
        let operator = caller_address();
        let mut hook = self.util.transfer(
            &operator,
            &params.to,
            &params.amount,
            params.operator_data,
            RawBytes::default(),
        )?;

        let cid = self.util.flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.util.msg())?;

        self.reload(&cid)?;
        let ret = self.util.transfer_return(hook_ret)?;

        Ok(ret)
    }

    fn transfer_from(
        &mut self,
        params: TransferFromParams,
    ) -> Result<TransferFromReturn, RuntimeError> {
        let operator = caller_address();
        let mut hook = self.util.transfer_from(
            &operator,
            &params.from,
            &params.to,
            &params.amount,
            params.operator_data,
            RawBytes::default(),
        )?;

        let cid = self.util.flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.util.msg())?;

        self.reload(&cid)?;
        let ret = self.util.transfer_from_return(hook_ret)?;

        Ok(ret)
    }

    fn increase_allowance(
        &mut self,
        params: IncreaseAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.util.increase_allowance(&owner, &params.operator, &params.increase)?;
        Ok(new_allowance)
    }

    fn decrease_allowance(
        &mut self,
        params: DecreaseAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.util.decrease_allowance(&owner, &params.operator, &params.decrease)?;
        Ok(new_allowance)
    }

    fn revoke_allowance(&mut self, params: RevokeAllowanceParams) -> Result<(), RuntimeError> {
        let owner = caller_address();
        self.util.revoke_allowance(&owner, &params.operator)?;
        Ok(())
    }

    fn allowance(&mut self, params: GetAllowanceParams) -> Result<AllowanceReturn, RuntimeError> {
        let allowance = self.util.allowance(&params.owner, &params.operator)?;
        Ok(allowance)
    }

    fn burn(&mut self, params: BurnParams) -> Result<BurnReturn, RuntimeError> {
        let caller = caller_address();
        let res = self.util.burn(&caller, &params.amount)?;
        Ok(res)
    }

    fn burn_from(
        &mut self,
        params: frc46_token::token::types::BurnFromParams,
    ) -> Result<BurnFromReturn, RuntimeError> {
        let caller = caller_address();
        let res = self.util.burn_from(&caller, &params.owner, &params.amount)?;
        Ok(res)
    }
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct MintParams {
    pub initial_owner: Address,
    pub amount: TokenAmount,
    pub operator_data: RawBytes,
}

impl Cbor for MintParams {}

impl BasicToken<'_> {
    fn reload(&mut self, initial_cid: &Cid) -> Result<(), RuntimeError> {
        // todo: revise error type here so it plays nice with the result and doesn't need unwrap
        let new_cid = sdk::sself::root().unwrap();
        if new_cid != *initial_cid {
            self.util.load_replace(&new_cid)?;
        }
        Ok(())
    }

    fn mint(&mut self, params: MintParams) -> Result<MintReturn, RuntimeError> {
        let mut hook = self.util.mint(
            &caller_address(),
            &params.initial_owner,
            &params.amount,
            params.operator_data,
            Default::default(),
        )?;

        let cid = self.util.flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.util.msg())?;

        self.reload(&cid)?;
        let ret = self.util.mint_return(hook_ret)?;

        Ok(ret)
    }
}

pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}

/// Generic invoke for FRC46 Token methods
/// Given a method number and parameter block id, invokes the appropriate method on the FRC46Token interface
/// Possible returns:
/// - Ok(None) - method not found or method returned no result (eg: RevokeAllowance)
/// - Ok(Some(RawBytes)) - results of method call serialized to RawBytes
/// - Err(error) - any error encountered during operation
///
/// TODO: 'method not found' and 'method returned no result', should return distinct results
///
/// TODO: some operations need to save state before returning, others need to do it before calling a receiver hook
/// how to deal with this? supply a separate 
fn frc46_invoke<T>(method_num: u64, params: u32, token: &mut T) -> Result<Option<RawBytes>, RuntimeError>
where
    T: FRC46Token<RuntimeError>,
{
    match_method!(method_num, {
        "Name" => {
            RawBytes::serialize(token.name()).map(Option::Some)
        }
        "Symbol" => {
            RawBytes::serialize(token.symbol()).map(Option::Some)
        }
        "TotalSupply" => {
            RawBytes::serialize(token.total_supply()).map(Option::Some)
        }
        "BalanceOf" => {
            let params = deserialize_params(params);
            let res = token.balance_of(params)?;
            RawBytes::serialize(res).map(Option::Some)
        }
        "Allowance" => {
            let params = deserialize_params(params);
            let res = token.allowance(params)?;
            RawBytes::serialize(res).map(Option::Some)
        }
        "IncreaseAllowance" => {
            let params = deserialize_params(params);
            let res = token.increase_allowance(params)?;
            // TODO: this needs to flush state to the blockstore before returning
            RawBytes::serialize(res).map(Option::Some)
        }
        "DecreaseAllowance" => {
            let params = deserialize_params(params);
            let res = token.decrease_allowance(params)?;
            // TODO: this needs to flush state to the blockstore before returning
            RawBytes::serialize(res).map(Option::Some)
        }
        "RevokeAllowance" => {
            let params = deserialize_params(params);
            token.revoke_allowance(params)?;
            // TODO: this needs to flush state to the blockstore before returning
            Ok(None)
        }
        "Burn" => {
            let params = deserialize_params(params);
            let res = token.burn(params)?;
            // TODO: this needs to flush state to the blockstore before returning
            RawBytes::serialize(res).map(Option::Some)

        }
        "TransferFrom" => {
            let params = deserialize_params(params);
            let res = token.transfer_from(params)?;
            RawBytes::serialize(res).map(Option::Some)
        }
        "Transfer" => {
            let params = deserialize_params(params);
            let res = token.transfer(params)?;
            RawBytes::serialize(res).map(Option::Some)
        }
        _ => {
            // TODO:this might need a separate result type or something
            // so we can tell the difference between an empty result (like RevokeAllowance) and method not found
            // this shouldn't be an error though as we're expecting to be used as part of some larger parent invoke() function
            Ok(None)
        }
    })
    .map_err(RuntimeError::from)
}
