use frc42_dispatch::match_method;
use fvm_actor_utils::blockstore::Blockstore;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_sdk::NO_DATA_BLOCK_ID;
use fvm_shared::{address::Address, error::ExitCode};
pub mod token;

use token::{
    deserialize_params, frc46_invoke, return_ipld, FactoryToken, MintParams, RuntimeError,
};

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ConstructorParams {
    pub name: String,
    pub symbol: String,
    pub granularity: u64,
    /// authorised mint operator
    /// only this address can mint tokens or remove themselves to permanently disable minting
    pub minter: Address,
}

fn construct_token(params: ConstructorParams) -> Result<u32, RuntimeError> {
    let bs = Blockstore::default();
    let token =
        FactoryToken::new(&bs, params.name, params.symbol, params.granularity, Some(params.minter));

    let cid = token.save()?;
    fvm_sdk::sself::set_root(&cid)?;

    Ok(NO_DATA_BLOCK_ID)
}

fn token_invoke(method_num: u64, params: u32) -> Result<u32, RuntimeError> {
    match_method!(method_num, {
        "Constructor" => {
            let params = deserialize_params(params);
            construct_token(params)
        }
        "Mint" => {
            let root_cid = fvm_sdk::sself::root()?;
            let params: MintParams = deserialize_params(params);
            let mut token_actor = FactoryToken::load(&root_cid)?;
            let res = token_actor.mint(params)?;
            return_ipld(&res)
        }
        "DisableMint" => {
            let root_cid = fvm_sdk::sself::root()?;
            let mut token_actor = FactoryToken::load(&root_cid)?;
            // disable minting forever
            token_actor.disable_mint()?;
            // save state
            let cid = token_actor.save()?;
            fvm_sdk::sself::set_root(&cid)?;
            // no return
            Ok(NO_DATA_BLOCK_ID)
        }
        _ => {
            let root_cid = fvm_sdk::sself::root()?;

            let mut token_actor = FactoryToken::load(&root_cid)?;

            // call FRC46 token methods
            // note that the `token_actor` passed in here needs to know how to save and load state
            let res = frc46_invoke(method_num, params, &mut token_actor, |token| {
                // `token` is passed through from the original token provided in the function call
                // so it won't break mutable borrow rules when used here (trying to use token_actor directly won't work)
                let cid = token.save()?;
                fvm_sdk::sself::set_root(&cid)?;
                Ok(())
            })?;
            match res {
                // handled by frc46_invoke, return result
                Some(r) => Ok(r),
                // method not found
                None => {
                    fvm_sdk::vm::abort(
                        ExitCode::USR_UNHANDLED_MESSAGE.value(),
                        Some("Unknown method number"),
                    )
                }
            }
        }
    })
}

#[no_mangle]
pub fn invoke(params: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        fvm_sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{}", info)))
    }));

    let method_num = fvm_sdk::message::method_number();
    match token_invoke(method_num, params) {
        Ok(ret) => ret,
        Err(err) => {
            fvm_sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&err.to_string()))
        }
    }
}
