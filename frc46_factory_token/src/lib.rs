use frc42_dispatch::match_method;
use fvm_actor_utils::{
    blockstore::Blockstore, syscalls::fvm_syscalls::FvmSyscalls, util::ActorRuntime,
};
use fvm_sdk::NO_DATA_BLOCK_ID;
use fvm_shared::error::ExitCode;
use token_impl::{
    construct_token, deserialize_params, frc46_invoke, return_ipld, FactoryToken, MintParams,
    RuntimeError,
};

fn token_invoke(method_num: u64, params: u32) -> Result<u32, RuntimeError> {
    match_method!(method_num, {
        "Constructor" => {
            let params = deserialize_params(params);
            let runtime = ActorRuntime::<FvmSyscalls, Blockstore>::new_fvm_runtime();
            construct_token(runtime, params)
        }
        "Mint" => {
            let root_cid = fvm_sdk::sself::root()?;
            let params: MintParams = deserialize_params(params);
            let runtime = ActorRuntime::<FvmSyscalls, Blockstore>::new_fvm_runtime();
            let mut token_actor = FactoryToken::load(runtime, &root_cid)?;
            let res = token_actor.mint(params)?;
            return_ipld(&res)
        }
        "DisableMint" => {
            let root_cid = fvm_sdk::sself::root()?;
            let runtime = ActorRuntime::<FvmSyscalls, Blockstore>::new_fvm_runtime();
            let mut token_actor = FactoryToken::load(runtime, &root_cid)?;
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

            let runtime = ActorRuntime::<FvmSyscalls, Blockstore>::new_fvm_runtime();
            let mut token_actor = FactoryToken::load(runtime, &root_cid)?;

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
        fvm_sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{info}")))
    }));

    let method_num = fvm_sdk::message::method_number();
    match token_invoke(method_num, params) {
        Ok(ret) => ret,
        Err(err) => fvm_sdk::vm::abort(ExitCode::from(&err).value(), Some(&err.to_string())),
    }
}
