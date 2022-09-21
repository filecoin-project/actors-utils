mod util;

use cid::Cid;
use frc46_token::token::types::{
    AllowanceReturn, BalanceReturn, BurnFromReturn, BurnParams, BurnReturn,
    DecreaseAllowanceParams, FRC46Token, GetAllowanceParams, GranularityReturn,
    IncreaseAllowanceParams, MintReturn, Result, RevokeAllowanceParams, TotalSupplyReturn,
    TransferFromReturn, TransferParams, TransferReturn,
};
use frc46_token::token::Token;
use fvm_actor_utils::blockstore::Blockstore;
use fvm_actor_utils::messaging::FvmMessenger;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes, DAG_CBOR};
use fvm_sdk as sdk;
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use sdk::sys::ErrorNumber;
use sdk::NO_DATA_BLOCK_ID;
use serde::ser;
use thiserror::Error;
use util::{caller_address, deserialize_params, RuntimeError};

struct BasicToken<'state> {
    /// Default token helper impl
    util: Token<'state, Blockstore, FvmMessenger>,
}

/// Implementation of the token API in a FVM actor
///
/// Here the Ipld parameter structs are marshalled and passed to the underlying library functions
impl FRC46Token<RuntimeError> for BasicToken<'_> {
    fn name(&self) -> String {
        String::from("FRC-0046 Token")
    }

    fn symbol(&self) -> String {
        String::from("FRC46")
    }

    fn granularity(&self) -> GranularityReturn {
        1
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
        params: frc46_token::token::types::TransferFromParams,
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

#[derive(Error, Debug)]
enum IpldError {
    #[error("ipld encoding error: {0}")]
    Encoding(#[from] fvm_ipld_encoding::Error),
    #[error("ipld blockstore error: {0}")]
    Blockstore(#[from] ErrorNumber),
}

fn return_ipld<T>(value: &T) -> std::result::Result<u32, IpldError>
where
    T: ser::Serialize + ?Sized,
{
    let bytes = fvm_ipld_encoding::to_vec(value)?;
    Ok(sdk::ipld::put_block(DAG_CBOR, bytes.as_slice())?)
}

/// Conduct method dispatch. Handle input parameters and return data.
#[no_mangle]
pub fn invoke(params: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{}", info)))
    }));

    let method_num = sdk::message::method_number();

    match method_num {
        // Actor constructor
        1 => constructor(),

        // Standard token interface
        rest => {
            let root_cid = sdk::sself::root().unwrap();

            let bs = Blockstore::default();
            let mut token_state = Token::<_, FvmMessenger>::load_state(&bs, &root_cid).unwrap();

            let mut token_actor =
                BasicToken { util: Token::wrap(bs, FvmMessenger::default(), 1, &mut token_state) };

            // Method numbers calculated via fvm_dispatch_tools using CamelCase names derived from
            // the corresponding FRC46Token trait methods.
            match rest {
                4244593718 => {
                    // Name
                    let name = token_actor.name();
                    return_ipld(&name).unwrap()
                }
                3551111368 => {
                    // Symbol
                    let symbol = token_actor.symbol();
                    return_ipld(&symbol).unwrap()
                }
                2511420746 => {
                    // TotalSupply
                    let total_supply = token_actor.total_supply();
                    return_ipld(&total_supply).unwrap()
                }
                1568445334 => {
                    //BalanceOf
                    let params = deserialize_params(params);
                    let res = token_actor.balance_of(params).unwrap();
                    return_ipld(&res).unwrap()
                }
                2804639308 => {
                    // Allowance
                    let params = deserialize_params(params);
                    let res = token_actor.allowance(params).unwrap();
                    return_ipld(&res).unwrap()
                }
                991449938 => {
                    // IncreaseAllowance
                    let params = deserialize_params(params);
                    let res = token_actor.increase_allowance(params).unwrap();
                    let cid = token_actor.util.flush().unwrap();
                    sdk::sself::set_root(&cid).unwrap();
                    return_ipld(&res).unwrap()
                }
                4218751446 => {
                    // DecreaseAllowance
                    let params = deserialize_params(params);
                    let res = token_actor.decrease_allowance(params).unwrap();
                    let cid = token_actor.util.flush().unwrap();
                    sdk::sself::set_root(&cid).unwrap();
                    return_ipld(&res).unwrap()
                }
                1691518633 => {
                    // RevokeAllowance
                    let params = deserialize_params(params);
                    token_actor.revoke_allowance(params).unwrap();
                    let cid = token_actor.util.flush().unwrap();
                    sdk::sself::set_root(&cid).unwrap();
                    NO_DATA_BLOCK_ID
                }
                1924391931 => {
                    // Burn
                    let params = deserialize_params(params);
                    let res = token_actor.burn(params).unwrap();
                    let cid = token_actor.util.flush().unwrap();
                    sdk::sself::set_root(&cid).unwrap();
                    return_ipld(&res).unwrap()
                }
                401872942 => {
                    // TransferFrom
                    let params = deserialize_params(params);
                    let res = token_actor.transfer_from(params).unwrap();
                    return_ipld(&res).unwrap()
                }
                1303003700 => {
                    // Transfer
                    let params = deserialize_params(params);
                    let res = token_actor.transfer(params).unwrap();
                    return_ipld(&res).unwrap()
                }

                // Custom actor interface, these are author-defined methods that extend beyond the
                // FRC46 Token standard
                3839021839 => {
                    // Mint
                    let params: MintParams = deserialize_params(params);
                    let res = token_actor.mint(params).unwrap();
                    return_ipld(&res).unwrap()
                }
                _ => {
                    sdk::vm::abort(
                        ExitCode::USR_UNHANDLED_MESSAGE.value(),
                        Some("Unknown method number"),
                    );
                }
            }
        }
    }
}

fn constructor() -> u32 {
    let bs = Blockstore::default();
    let mut token_state = Token::<_, FvmMessenger>::create_state(&bs).unwrap();
    let mut token = Token::wrap(bs, FvmMessenger::default(), 1, &mut token_state);
    let cid = token.flush().unwrap();
    sdk::sself::set_root(&cid).unwrap();
    NO_DATA_BLOCK_ID
}
