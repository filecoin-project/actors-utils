mod util;

use fil_fungible_token::runtime::blockstore::Blockstore;
use fil_fungible_token::runtime::messaging::FvmMessenger;
use fil_fungible_token::token::types::{
    AllowanceReturn, BurnParams, BurnReturn, ChangeAllowanceParams, FrcXXXToken,
    GetAllowanceParams, Result, RevokeAllowanceParams, TransferParams, TransferReturn,
};
use fil_fungible_token::token::Token;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_sdk as sdk;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::econ::TokenAmount;
use num_traits::Zero;
use sdk::sys::ErrorNumber;
use sdk::NO_DATA_BLOCK_ID;
use serde::ser;
use thiserror::Error;
use util::{caller_address, deserialize_params, RuntimeError};

struct BasicToken {
    /// Default token helper impl
    util: Token<Blockstore, FvmMessenger>,
}

/// Implementation of the token API in a FVM actor
///
/// Here the Ipld parameter structs are marshalled and passed to the underlying library functions
impl FrcXXXToken<RuntimeError> for BasicToken {
    fn name(&self) -> String {
        String::from("FRC XXX Token")
    }

    fn symbol(&self) -> String {
        String::from("FRCXXX")
    }

    fn total_supply(&self) -> TokenAmount {
        self.util.total_supply()
    }

    fn balance_of(
        &self,
        params: fvm_shared::address::Address,
    ) -> Result<TokenAmount, RuntimeError> {
        Ok(self.util.balance_of(&params)?)
    }

    fn increase_allowance(
        &mut self,
        params: ChangeAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.util.increase_allowance(&owner, &params.operator, &params.amount)?;
        Ok(AllowanceReturn { owner, operator: params.operator, amount: new_allowance })
    }

    fn decrease_allowance(
        &mut self,
        params: ChangeAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        let new_allowance =
            self.util.decrease_allowance(&owner, &params.operator, &params.amount)?;
        Ok(AllowanceReturn { owner, operator: params.operator, amount: new_allowance })
    }

    fn revoke_allowance(
        &mut self,
        params: RevokeAllowanceParams,
    ) -> Result<AllowanceReturn, RuntimeError> {
        let owner = caller_address();
        self.util.revoke_allowance(&owner, &params.operator)?;
        Ok(AllowanceReturn { owner, operator: params.operator, amount: TokenAmount::zero() })
    }

    fn allowance(&mut self, params: GetAllowanceParams) -> Result<AllowanceReturn, RuntimeError> {
        let allowance = self.util.allowance(&params.owner, &params.operator)?;
        Ok(AllowanceReturn { owner: params.owner, operator: params.operator, amount: allowance })
    }

    fn burn(&mut self, params: BurnParams) -> Result<BurnReturn, RuntimeError> {
        let spender = caller_address();
        let remaining = self.util.burn(&spender, &params.owner, &params.amount)?;
        Ok(BurnReturn {
            by: spender,
            remaining_balance: remaining,
            burnt: params.amount.clone(),
            owner: params.owner,
        })
    }

    fn transfer(&mut self, params: TransferParams) -> Result<TransferReturn, RuntimeError> {
        let spender = caller_address();
        self.util.transfer(
            &caller_address(),
            &params.from,
            &params.to,
            &params.amount,
            &params.data,
        )?;
        Ok(TransferReturn {
            from: params.from,
            to: params.to,
            by: spender,
            amount: params.amount.clone(),
        })
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
    let method_num = sdk::message::method_number();

    match method_num {
        // Actor constructor
        1 => constructor(),

        // Standard token interface
        rest => {
            let root_cid = sdk::sself::root().unwrap();
            let mut token_actor = BasicToken {
                util: Token::load(Blockstore::default(), FvmMessenger::default(), root_cid)
                    .unwrap(),
            };

            // Method numbers calculated via fvm_dispatch_tools using CamelCase names derived from
            // the corresponding FRCXXXToken trait methods.
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
                    return_ipld(&BigIntDe(total_supply)).unwrap()
                }
                1568445334 => {
                    //BalanceOf
                    let params = deserialize_params(params);
                    let res = token_actor.balance_of(params).unwrap();
                    return_ipld(&BigIntDe(res)).unwrap()
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
                    token_actor.util.flush().unwrap();
                    return_ipld(&res).unwrap()
                }
                4218751446 => {
                    // DecreaseAllowance
                    let params = deserialize_params(params);
                    let res = token_actor.decrease_allowance(params).unwrap();
                    token_actor.util.flush().unwrap();
                    return_ipld(&res).unwrap()
                }
                1691518633 => {
                    // RevokeAllowance
                    let params = deserialize_params(params);
                    let res = token_actor.revoke_allowance(params).unwrap();
                    token_actor.util.flush().unwrap();
                    return_ipld(&res).unwrap()
                }
                1924391931 => {
                    // Burn
                    let params = deserialize_params(params);
                    let res = token_actor.burn(params).unwrap();
                    token_actor.util.flush().unwrap();
                    return_ipld(&res).unwrap()
                }
                401872942 => {
                    // TransferFrom
                    let params = deserialize_params(params);
                    let res = token_actor.transfer(params).unwrap();
                    token_actor.util.flush().unwrap();
                    return_ipld(&res).unwrap()
                }

                // Custom actor interface, these are author-defined methods that extend beyond the
                // FRCXXX Token standard
                3839021839 => {
                    // Mint
                    // This is an exmaple mint function which simply gives the caller 100 tokens
                    let minter = caller_address();
                    token_actor
                        .util
                        .mint(&minter, &minter, &TokenAmount::from(100), &Default::default())
                        .unwrap();
                    token_actor.util.flush().unwrap();
                    NO_DATA_BLOCK_ID
                }
                _ => {
                    sdk::vm::abort(
                        fvm_shared::error::ExitCode::USR_ILLEGAL_ARGUMENT.value(),
                        Some("Unknown method number"),
                    );
                }
            }
        }
    }
}

fn constructor() -> u32 {
    let (_token, cid) = Token::new(Blockstore::default(), FvmMessenger::default()).unwrap();
    sdk::sself::set_root(&cid).unwrap();
    NO_DATA_BLOCK_ID
}
