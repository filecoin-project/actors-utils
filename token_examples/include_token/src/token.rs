use cid::{multihash::Code, Cid};
use frc42_dispatch::match_method;
use frc46_token::token::{
    state::{StateError, TokenState},
    types::{
        AllowanceReturn, BalanceReturn, BurnFromReturn, BurnParams, BurnReturn,
        DecreaseAllowanceParams, FRC46Token, GetAllowanceParams, GranularityReturn,
        IncreaseAllowanceParams, MintReturn, RevokeAllowanceParams, TotalSupplyReturn,
        TransferFromParams, TransferFromReturn, TransferParams, TransferReturn,
    },
    Token, TokenError,
};
use fvm_actor_utils::{
    blockstore::Blockstore,
    messaging::{FvmMessenger, Messaging, MessagingError},
    receiver::ReceiverHookError,
};
use fvm_ipld_blockstore::{Block, Blockstore as _BS};
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    Cbor, CborStore, RawBytes, DAG_CBOR,
};
use fvm_sdk::{self as sdk, error::NoStateError, sys::ErrorNumber, NO_DATA_BLOCK_ID};
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount};
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
    // deserialisation error when loading state
    #[error("error loading state {0}")]
    Deserialization(String),
    // serialisation error when saving state
    #[error("error saving state {0}")]
    Serialization(String),
    #[error("underlying state error {0}")]
    State(#[from] StateError),
    #[error("actor messaging error {0}")]
    Messaging(#[from] MessagingError),
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
#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct BasicToken {
    /// Default token helper impl
    pub token: TokenState,
    /// basic token identifier stuff, should it go here or store separately alongside the state
    pub name: String,
    pub symbol: String,
    pub granularity: u64,
}

/// Implementation of the token API in a FVM actor
///
/// Here the Ipld parameter structs are marshalled and passed to the underlying library functions
impl FRC46Token<RuntimeError> for BasicToken {
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
        // TODO: token() wants mutable ref, we don't have one here and can't change the interface
        // so need an immutable wrapper to call these things?
        // or just bypass it and go to the state directly
        //self.token().total_supply()
        self.token.supply.clone()
    }

    fn balance_of(&self, params: Address) -> Result<BalanceReturn, RuntimeError> {
        // TODO: same situation as total_supply
        //Ok(self.token().balance_of(&params)?)
        let bs = Blockstore::default();
        let msg = FvmMessenger::default();
        match msg.resolve_id(&params) {
            Ok(owner) => Ok(self.token.get_balance(&bs, owner)?),
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero balance
                Ok(TokenAmount::zero())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn transfer(&mut self, params: TransferParams) -> Result<TransferReturn, RuntimeError> {
        let operator = caller_address();
        let mut hook = self.token().transfer(
            &operator,
            &params.to,
            &params.amount,
            params.operator_data,
            RawBytes::default(),
        )?;

        let cid = self.token().flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.token().msg())?;

        self.reload(&cid)?;
        let ret = self.token().transfer_return(hook_ret)?;

        Ok(ret)
    }

    fn transfer_from(
        &mut self,
        params: TransferFromParams,
    ) -> Result<TransferFromReturn, RuntimeError> {
        let operator = caller_address();
        let mut hook = self.token().transfer_from(
            &operator,
            &params.from,
            &params.to,
            &params.amount,
            params.operator_data,
            RawBytes::default(),
        )?;

        let cid = self.token().flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.token().msg())?;

        self.reload(&cid)?;
        let ret = self.token().transfer_from_return(hook_ret)?;

        Ok(ret)
    }

    fn increase_allowance(
        &mut self,
        params: IncreaseAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.token().increase_allowance(&owner, &params.operator, &params.increase)?;
        Ok(new_allowance)
    }

    fn decrease_allowance(
        &mut self,
        params: DecreaseAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.token().decrease_allowance(&owner, &params.operator, &params.decrease)?;
        Ok(new_allowance)
    }

    fn revoke_allowance(&mut self, params: RevokeAllowanceParams) -> Result<(), RuntimeError> {
        let owner = caller_address();
        self.token().revoke_allowance(&owner, &params.operator)?;
        Ok(())
    }

    fn allowance(&mut self, params: GetAllowanceParams) -> Result<AllowanceReturn, RuntimeError> {
        let allowance = self.token().allowance(&params.owner, &params.operator)?;
        Ok(allowance)
    }

    fn burn(&mut self, params: BurnParams) -> Result<BurnReturn, RuntimeError> {
        let caller = caller_address();
        let res = self.token().burn(&caller, &params.amount)?;
        Ok(res)
    }

    fn burn_from(
        &mut self,
        params: frc46_token::token::types::BurnFromParams,
    ) -> Result<BurnFromReturn, RuntimeError> {
        let caller = caller_address();
        let res = self.token().burn_from(&caller, &params.owner, &params.amount)?;
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
impl BasicToken {
    pub fn new(name: String, symbol: String, granularity: u64) -> Self {
        let bs = Blockstore::default();
        BasicToken { token: TokenState::new(&bs).unwrap(), name, symbol, granularity }
    }

    pub fn token(&mut self) -> Token<'_, Blockstore, FvmMessenger> {
        let bs = Blockstore::default();
        let msg = FvmMessenger::default();
        Token::wrap(bs, msg, self.granularity, &mut self.token)
    }

    pub fn load(cid: &Cid) -> Result<Self, RuntimeError> {
        let bs = Blockstore::default();
        match bs.get_cbor::<Self>(cid) {
            Ok(Some(s)) => Ok(s),
            // TODO: improve on these errors?
            Ok(None) => Err(RuntimeError::Deserialization("no data found".into())),
            Err(e) => Err(RuntimeError::Deserialization(e.to_string())),
        }
    }

    pub fn save(&self) -> Result<Cid, RuntimeError> {
        let bs = Blockstore::default();
        let serialized = fvm_ipld_encoding::to_vec(self)
            .map_err(|err| RuntimeError::Serialization(err.to_string()))?;
        let block = Block { codec: DAG_CBOR, data: serialized };
        bs.put(Code::Blake2b256, &block).map_err(|err| RuntimeError::Serialization(err.to_string()))
    }

    fn reload(&mut self, initial_cid: &Cid) -> Result<(), RuntimeError> {
        let new_cid = sdk::sself::root()?;
        if new_cid != *initial_cid {
            self.token().load_replace(&new_cid)?;
        }
        Ok(())
    }

    pub fn mint(&mut self, params: MintParams) -> Result<MintReturn, RuntimeError> {
        let mut hook = self.token().mint(
            &caller_address(),
            &params.initial_owner,
            &params.amount,
            params.operator_data,
            Default::default(),
        )?;

        let cid = self.token().flush()?;
        sdk::sself::set_root(&cid).unwrap();

        let hook_ret = hook.call(self.token().msg())?;

        self.reload(&cid)?;
        let ret = self.token().mint_return(hook_ret)?;

        Ok(ret)
    }
}

pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}

pub fn return_ipld<T>(value: &T) -> std::result::Result<u32, RuntimeError>
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
