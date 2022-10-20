use frc42_dispatch::match_method;
use frc46_token::token::Token;
use fvm_actor_utils::{
    blockstore::Blockstore, messaging::FvmMessenger, receiver::ReceiverHookError,
};
use fvm_sdk::NO_DATA_BLOCK_ID;
use fvm_shared::error::ExitCode;
mod token;

use token::{frc46_invoke, BasicToken};

fn construct_token() {
    let bs = Blockstore::default();
    // TODO: need to construct a BasicToken and store that, not only the TokenState
    let mut token_state = Token::<_, FvmMessenger>::create_state(&bs).unwrap();
    let mut token = Token::wrap(bs, FvmMessenger::default(), 1, &mut token_state);
    let cid = token.flush().unwrap();
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
            construct_token();
            NO_DATA_BLOCK_ID
        }
        "Mint" => {
            fvm_sdk::vm::abort(
                ExitCode::USR_UNHANDLED_MESSAGE.value(),
                Some("Unknown method number"),
            )
        }
        _ => {
            let root_cid = fvm_sdk::sself::root().unwrap();

            let bs = Blockstore::default();
            // TODO: we need to load (and later store) more than just this basic state now
            let mut token_state = Token::<_, FvmMessenger>::load_state(&bs, &root_cid).unwrap();

            let mut token_actor =
                BasicToken { util: Token::wrap(bs, FvmMessenger::default(), 1, &mut token_state), name: String::from("Test Token"), symbol: String::from("TEST"), granularity: 1 };

            // call FRC46 token methods
            // note that the `token_actor` passed in here needs to know how to save and load state
            let res = frc46_invoke(method_num, params, &mut token_actor, |token| {
                // `token` is passed through from the original token provided in the function call
                // so it won't break mutable borrow rules when used here (trying to use token_actor directly won't work)
                let cid = token.util.flush()?; // TODO: we need to store the entire BasicToken now, so util.flush() won't be enough
                fvm_sdk::sself::set_root(&cid)?;
                Ok(())
            }).unwrap();
            match res {
                // handled by frc46_invoke, return result
                Some(r) => r,
                // method not found
                None => fvm_sdk::vm::abort(
                        ExitCode::USR_UNHANDLED_MESSAGE.value(),
                        Some("Unknown method number"),
                    )
            }
        }
    })
}
