use frc42_dispatch::match_method;
use fvm_actor_utils::blockstore::Blockstore;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_sdk::NO_DATA_BLOCK_ID;
use fvm_shared::error::ExitCode;
pub mod token;

use token::{deserialize_params, frc46_invoke, return_ipld, BasicToken, MintParams};

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ConstructorParams {
    pub name: String,
    pub symbol: String,
    pub granularity: u64,
    // TODO: minting strategy stuff
}

fn construct_token(params: ConstructorParams) {
    let bs = Blockstore::default();
    let token = BasicToken::new(&bs, params.name, params.symbol, params.granularity);
    let cid = token.save().unwrap();
    fvm_sdk::sself::set_root(&cid).unwrap();
}

#[no_mangle]
pub fn invoke(params: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        fvm_sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{}", info)))
    }));

    let method_num = fvm_sdk::message::method_number();
    // we only implement our own methods in this handler, anything we don't handle directly is delegated to frc46_invoke
    // which handles any methods in the FRC46 token interface to save us writing the same
    match_method!(method_num, {
        "Constructor" => {
            let params = deserialize_params(params);
            construct_token(params);
            NO_DATA_BLOCK_ID
        }
        "Mint" => {
            let root_cid = fvm_sdk::sself::root().unwrap();
            let params: MintParams = deserialize_params(params);
            let mut token_actor = BasicToken::load(&root_cid).unwrap();
            let res = token_actor.mint(params).unwrap();
            return_ipld(&res).unwrap()
        }
        _ => {
            let root_cid = fvm_sdk::sself::root().unwrap();

            let mut token_actor = BasicToken::load(&root_cid).unwrap();

            // call FRC46 token methods
            // note that the `token_actor` passed in here needs to know how to save and load state
            let res = frc46_invoke(method_num, params, &mut token_actor, |token| {
                // `token` is passed through from the original token provided in the function call
                // so it won't break mutable borrow rules when used here (trying to use token_actor directly won't work)
                let cid = token.token().flush()?; // TODO: we need to store the entire BasicToken now, so util.flush() won't be enough
                fvm_sdk::sself::set_root(&cid)?;
                Ok(())
            }).unwrap();
            match res {
                // handled by frc46_invoke, return result
                Some(r) => r,
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
