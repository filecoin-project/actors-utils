use frc42_dispatch::{match_method, method_hash};
use frc46_token::{
    receiver::types::{FRC46TokenReceived, UniversalReceiverParams, FRC46_TOKEN_TYPE},
    token::types::{BurnParams, TransferParams},
};
use fvm_ipld_encoding::{
    de::DeserializeOwned,
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes,
};
use fvm_sdk as sdk;
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount, error::ExitCode};
use sdk::NO_DATA_BLOCK_ID;
use serde::{Deserialize, Serialize};

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}

/// Action to take in receiver hook or Action method
/// This gets serialized and sent along as operator_data
#[derive(Serialize, Deserialize, Debug)]
pub enum TestAction {
    /// Accept the tokens
    Accept,
    /// Reject the tokens (hook aborts)
    Reject,
    /// Transfer to another address (with operator_data that can provide further instructions)
    Transfer(Address, RawBytes),
    /// Burn incoming tokens
    Burn,
}

/// Params for Action method call
#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ActionParams {
    /// Address of the token actor
    token_address: Address,
    /// Action to take with our token balance. Only Transfer and Burn actions apply here.
    action: TestAction,
}

#[no_mangle]
fn invoke(input: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{}", info)))
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
            match action {
                TestAction::Accept => {
                    // do nothing, return success
                }
                TestAction::Reject => {
                    // abort to reject transfer
                    sdk::vm::abort(
                        ExitCode::USR_FORBIDDEN.value(),
                        Some("rejecting transfer"),
                    );
                }
                TestAction::Transfer(to, operator_data) => {
                    // transfer to a target address
                    let transfer_params = TransferParams {
                        to,
                        amount: token_params.amount,
                        operator_data,
                    };
                    let receipt = sdk::send::send(&Address::new_id(sdk::message::caller()), method_hash!("Transfer"), RawBytes::serialize(&transfer_params).unwrap(), TokenAmount::zero()).unwrap();
                    if !receipt.exit_code.is_success() {
                        panic!("transfer call failed");
                    }
                }
                TestAction::Burn => {
                    // burn the tokens
                    let burn_params = BurnParams {
                        amount: token_params.amount,
                    };
                    let receipt = sdk::send::send(&Address::new_id(sdk::message::caller()), method_hash!("Burn"), RawBytes::serialize(&burn_params).unwrap(), TokenAmount::zero()).unwrap();
                    if !receipt.exit_code.is_success() {
                        panic!("burn call failed");
                    }
                }
            }

            // all good, don't need to return anything
            NO_DATA_BLOCK_ID
        },
        "Action" => {
            // take action independent of the receiver hook
            let params: ActionParams = deserialize_params(input);

            // get our balance
            let get_balance = || {
                let self_address = Address::new_id(sdk::message::receiver());
                let balance_receipt = sdk::send::send(&params.token_address, method_hash!("BalanceOf"), RawBytes::serialize(self_address).unwrap(), TokenAmount::zero()).unwrap();
                if !balance_receipt.exit_code.is_success() {
                    panic!("unable to get balance");
                }
                balance_receipt.return_data.deserialize::<TokenAmount>().unwrap()
            };

            match params.action {
                TestAction::Accept => {
                    // nothing to do here
                }
                TestAction::Reject => {
                    // nothing to do here
                }
                TestAction::Transfer(to, operator_data) => {
                    // transfer to a target address
                    let balance = get_balance();
                    let transfer_params = TransferParams {
                        to,
                        amount: balance,
                        operator_data,
                    };
                    let receipt = sdk::send::send(&params.token_address, method_hash!("Transfer"), RawBytes::serialize(&transfer_params).unwrap(), TokenAmount::zero()).unwrap();
                    if !receipt.exit_code.is_success() {
                        panic!("transfer call failed");
                    }
                }
                TestAction::Burn => {
                    // burn the tokens
                    let balance = get_balance();
                    let burn_params = BurnParams {
                        amount: balance,
                    };
                    let receipt = sdk::send::send(&params.token_address, method_hash!("Burn"), RawBytes::serialize(&burn_params).unwrap(), TokenAmount::zero()).unwrap();
                    if !receipt.exit_code.is_success() {
                        panic!("burn call failed");
                    }
                }
            }

            // all good, don't need to return anything
            NO_DATA_BLOCK_ID
        }
        _ => {
            sdk::vm::abort(
                ExitCode::USR_UNHANDLED_MESSAGE.value(),
                Some("Unknown method number"),
            );
        }
    })
}
