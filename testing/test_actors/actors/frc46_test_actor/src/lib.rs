use frc42_dispatch::{match_method, method_hash};
use frc46_token::{
    receiver::{FRC46TokenReceived, FRC46_TOKEN_TYPE},
    token::types::{BurnParams, TransferParams},
};
use fvm_actor_utils::receiver::UniversalReceiverParams;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_ipld_encoding::{
    de::DeserializeOwned,
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes, DAG_CBOR,
};
use fvm_sdk as sdk;
use fvm_shared::receipt::Receipt;
use fvm_shared::sys::SendFlags;
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount, error::ExitCode};
use sdk::NO_DATA_BLOCK_ID;
use serde::{Deserialize, Serialize};

/// Grab the incoming parameters and convert from RawBytes to deserialized struct.
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().unwrap();
    let params = RawBytes::new(params.data);
    params.deserialize().unwrap()
}

fn return_ipld<T>(value: &T) -> u32
where
    T: Serialize + ?Sized,
{
    let bytes = fvm_ipld_encoding::to_vec(value).unwrap();
    sdk::ipld::put_block(DAG_CBOR, &bytes).unwrap()
}

/// Action to take in receiver hook or Action method.
///
/// This gets serialized and sent along as [`TransferParams::operator_data`].
#[derive(Serialize, Deserialize, Debug)]
pub enum TestAction {
    /// Accept the tokens.
    Accept,
    /// Reject the tokens (hook aborts).
    Reject,
    /// Transfer to another address (with operator_data that can provide further instructions).
    Transfer(Address, RawBytes),
    /// Burn incoming tokens.
    Burn,
    /// Take action, then abort afterwards.
    ActionThenAbort(RawBytes),
    /// Transfer to another address (with instructions for recipient), but take alternative action if rejected.
    TransferWithFallback { to: Address, instructions: RawBytes, fallback: RawBytes },
}

/// Params for Action method call.
///
/// This gives us a way to supply the token address, since we won't get it as a sender like we do
/// for hook calls.
#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ActionParams {
    /// Address of the token actor.
    pub token_address: Address,
    /// Action to take with our token balance. Only [`Transfer`][`TestAction::Transfer`] and
    /// [`TestAction::Burn`] actions apply here.
    pub action: TestAction,
}

/// Helper for nesting calls to create action sequences.
///
/// E.g., transfer and then the receiver hook rejects:
///
/// ```ignore
/// action(TestAction::Transfer(some_address, action(TestAction::Reject)))
/// ```
pub fn action(action: TestAction) -> RawBytes {
    RawBytes::serialize(action).unwrap()
}

/// Execute the Transfer action.
fn transfer(token: Address, to: Address, amount: TokenAmount, operator_data: RawBytes) -> Receipt {
    let transfer_params = TransferParams { to, amount, operator_data };
    let ret = sdk::send::send(
        &token,
        method_hash!("Transfer"),
        IpldBlock::serialize_cbor(&transfer_params).unwrap(),
        TokenAmount::zero(),
        None,
        SendFlags::empty(),
    )
    .unwrap();
    // ignore failures at this level and return the transfer call receipt so caller can decide what to do
    Receipt {
        exit_code: ret.exit_code,
        return_data: ret.return_data.map_or(RawBytes::default(), |b| RawBytes::new(b.data)),
        gas_used: 0,
        events_root: None,
    }
}

/// Execute the Burn action.
fn burn(token: Address, amount: TokenAmount) -> u32 {
    let burn_params = BurnParams { amount };
    let ret = sdk::send::send(
        &token,
        method_hash!("Burn"),
        IpldBlock::serialize_cbor(&burn_params).unwrap(),
        TokenAmount::zero(),
        None,
        SendFlags::empty(),
    )
    .unwrap();
    if !ret.exit_code.is_success() {
        panic!("burn call failed");
    }
    NO_DATA_BLOCK_ID
}

// handle a TestAction, which could possibly recurse in transfer-with-fallback or action-then-abort cases
fn handle_action(action: TestAction, token_address: Address) -> u32 {
    // get our balance
    let get_balance = || {
        let self_address = Address::new_id(sdk::message::receiver());
        let balance_ret = sdk::send::send(
            &token_address,
            method_hash!("BalanceOf"),
            IpldBlock::serialize_cbor(&self_address).unwrap(),
            TokenAmount::zero(),
            None,
            SendFlags::empty(),
        )
        .unwrap();
        if !balance_ret.exit_code.is_success() {
            panic!("unable to get balance");
        }
        balance_ret.return_data.unwrap().deserialize::<TokenAmount>().unwrap()
    };

    match action {
        TestAction::Accept | TestAction::Reject => {
            sdk::vm::abort(ExitCode::USR_ILLEGAL_ARGUMENT.value(), Some("invalid argument"));
        }
        TestAction::Transfer(to, operator_data) => {
            // transfer to a target address
            let balance = get_balance();
            let receipt = transfer(token_address, to, balance, operator_data);
            return_ipld(&receipt)
        }
        TestAction::Burn => {
            // burn the tokens
            let balance = get_balance();
            burn(token_address, balance)
        }
        TestAction::ActionThenAbort(action) => {
            let action: TestAction = action.deserialize().unwrap();
            handle_action(action, token_address);
            sdk::vm::abort(ExitCode::USR_UNSPECIFIED.value(), Some("aborted after test action"));
        }
        TestAction::TransferWithFallback { to, instructions, fallback } => {
            let balance = get_balance();
            let receipt = transfer(token_address, to, balance, instructions);
            // if transfer failed, try the fallback
            if !receipt.exit_code.is_success() {
                let fallback_action: TestAction = fallback.deserialize().unwrap();
                handle_action(fallback_action, token_address)
            } else {
                return_ipld(&receipt)
            }
        }
    }
}

fn handle_receive_action(action: TestAction, token_address: Address, amount: TokenAmount) -> u32 {
    match action {
        TestAction::Accept => {
            // do nothing, return success
            NO_DATA_BLOCK_ID
        }
        TestAction::Reject => {
            // abort to reject transfer
            sdk::vm::abort(ExitCode::USR_FORBIDDEN.value(), Some("rejecting transfer"));
        }
        TestAction::Transfer(to, operator_data) => {
            // transfer to a target address
            let receipt = transfer(token_address, to, amount, operator_data);
            return_ipld(&receipt)
        }
        TestAction::Burn => {
            // burn the tokens
            burn(Address::new_id(sdk::message::caller()), amount)
        }
        TestAction::ActionThenAbort(action) => {
            let action: TestAction = action.deserialize().unwrap();
            handle_receive_action(action, token_address, amount);
            sdk::vm::abort(ExitCode::USR_UNSPECIFIED.value(), Some("aborted after test action"));
        }
        TestAction::TransferWithFallback { to, instructions, fallback } => {
            let receipt = transfer(token_address, to, amount.clone(), instructions);
            // if transfer failed, try the fallback
            if !receipt.exit_code.is_success() {
                let fallback_action: TestAction = fallback.deserialize().unwrap();
                handle_receive_action(fallback_action, token_address, amount)
            } else {
                return_ipld(&receipt)
            }
        }
    }
}

#[no_mangle]
fn invoke(input: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{info}")))
    }));

    let method_num = sdk::message::method_number();
    match_method!(method_num, {
        "Constructor" => {
            NO_DATA_BLOCK_ID
        },
        "Receive" => {
            // Received is passed a UniversalReceiverParams
            let params: UniversalReceiverParams = deserialize_params(input);

            // reject if not an FRC46 token
            // we don't know how to inspect other payloads here
            if params.type_ != FRC46_TOKEN_TYPE {
                panic!("invalid token type, rejecting transfer");
            }

            // get token transfer data
            let token_params: FRC46TokenReceived = params.payload.deserialize().unwrap();

            // todo: examine the operator_data to determine our next move
            let action: TestAction = token_params.operator_data.deserialize().unwrap();
            handle_receive_action(action, Address::new_id(sdk::message::caller()), token_params.amount)
        },
        "Action" => {
            // take action independent of the receiver hook
            let params: ActionParams = deserialize_params(input);

            handle_action(params.action, params.token_address)
        }
        _ => {
            sdk::vm::abort(
                ExitCode::USR_UNHANDLED_MESSAGE.value(),
                Some("Unknown method number"),
            );
        }
    })
}
