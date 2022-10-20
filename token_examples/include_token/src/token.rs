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
use fvm_sdk::{self as sdk, error::NoStateError, sys::ErrorNumber, NO_DATA_BLOCK_ID};
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
    #[error("ipld blockstore error: {0}")]
    Blockstore(#[from] ErrorNumber),
    #[error("actor state not found {0}")]
    NoState(#[from] NoStateError),
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

pub struct BasicToken<'state> {
    /// Default token helper impl
    pub util: Token<'state, Blockstore, FvmMessenger>,
    /// basic token identifier stuff, should it go here or store separately alongside the state
    pub name: String,
    pub symbol: String,
    pub granularity: u64,
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

// TODO: add some state save/load/reload things here?
// or am i looking at things the wrong way?
// if BasicToken is a user implementation here then i don't need to worry about it so much
// i should move the load/save stuff here though, then FRC46Token methods can call into it
// it won't bother the invoke helper though, all it does is translate the incoming method number+params to token interface calls
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

fn return_ipld<T>(value: &T) -> std::result::Result<u32, RuntimeError>
where
    T: Serialize + ?Sized,
{
    let bytes = fvm_ipld_encoding::to_vec(value)?;
    Ok(sdk::ipld::put_block(DAG_CBOR, bytes.as_slice())?)
}

/// Generic invoke for FRC46 Token methods
/// Given a method number and parameter block id, invokes the appropriate method on the FRC46Token interface
///
/// The flush_state function passed into this must flush current state to the blockstore and update the root cid
/// This is called after operations which mutate the state, such as changing an allowance or burning tokens.
///
/// Transfer and TransferFrom operations invoke the receiver hook which will require flushing state before calling the hook
/// This must be done inside the FRC46Token::transfer/transfer_from functions
///
/// Possible returns:
/// - Ok(None) - method not found
/// - Ok(Some(u32)) - block id of results saved to blockstore (or NO_DATA_BLOCK_ID if there is no result to return)
/// - Err(error) - any error encountered during operation
///
pub fn frc46_invoke<T, F>(
    method_num: u64,
    params: u32,
    token: &mut T,
    flush_state: F,
) -> Result<Option<u32>, RuntimeError>
where
    T: FRC46Token<RuntimeError>,
    F: FnOnce(&mut T) -> Result<(), RuntimeError>,
{
    match_method!(method_num, {
        "Name" => {
            return_ipld(&token.name()).map(Option::Some)
        }
        "Symbol" => {
            return_ipld(&token.symbol()).map(Option::Some)
        }
        "TotalSupply" => {
            return_ipld(&token.total_supply()).map(Option::Some)
        }
        "BalanceOf" => {
            let params = deserialize_params(params);
            let res = token.balance_of(params)?;
            return_ipld(&res).map(Option::Some)
        }
        "Allowance" => {
            let params = deserialize_params(params);
            let res = token.allowance(params)?;
            return_ipld(&res).map(Option::Some)
        }
        "IncreaseAllowance" => {
            let params = deserialize_params(params);
            let res = token.increase_allowance(params)?;
            flush_state(token)?;
            return_ipld(&res).map(Option::Some)
        }
        "DecreaseAllowance" => {
            let params = deserialize_params(params);
            let res = token.decrease_allowance(params)?;
            flush_state(token)?;
            return_ipld(&res).map(Option::Some)
        }
        "RevokeAllowance" => {
            let params = deserialize_params(params);
            token.revoke_allowance(params)?;
            flush_state(token)?;
            Ok(Some(NO_DATA_BLOCK_ID))
        }
        "Burn" => {
            let params = deserialize_params(params);
            let res = token.burn(params)?;
            flush_state(token)?;
            return_ipld(&res).map(Option::Some)

        }
        "TransferFrom" => {
            let params = deserialize_params(params);
            let res = token.transfer_from(params)?;
            return_ipld(&res).map(Option::Some)
        }
        "Transfer" => {
            let params = deserialize_params(params);
            let res = token.transfer(params)?;
            return_ipld(&res).map(Option::Some)
        }
        _ => {
            // no method found - it's not considered an error here, but an upstream caller may choose to treat it as one
            Ok(None)
        }
    })
    .map_err(RuntimeError::from)
}
