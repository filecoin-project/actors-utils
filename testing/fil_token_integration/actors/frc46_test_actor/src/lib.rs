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
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount, error::ExitCode};
use sdk::NO_DATA_BLOCK_ID;
use serde::{Deserialize, Serialize};

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
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
/// This gives us a way to supply the token address, since we won't get it as a sender like we do for hook calls
#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ActionParams {
    /// Address of the token actor
    pub token_address: Address,
    /// Action to take with our token balance. Only Transfer and Burn actions apply here.
    pub action: TestAction,
}

/// Helper for nesting calls to create action sequences
/// eg. transfer and then the receiver hook rejects:
/// action(TestAction::Transfer(
///         some_address,
///         action(TestAction::Reject),
///     ),
/// )
pub fn action(action: TestAction) -> RawBytes {
    RawBytes::serialize(action).unwrap()
}

/// Execute the Transfer action
fn transfer(token: Address, to: Address, amount: TokenAmount, operator_data: RawBytes) -> u32 {
    let transfer_params = TransferParams { to, amount, operator_data };
    let ret = sdk::send::send(
        &token,
        method_hash!("Transfer"),
        IpldBlock::serialize_cbor(&transfer_params).unwrap(),
        TokenAmount::zero(),
    )
    .unwrap();
    // ignore failures at this level and return the transfer call receipt so caller can decide what to do
    return_ipld(&Receipt {
        exit_code: ret.exit_code,
        return_data: ret.return_data.map_or(RawBytes::default(), |b| RawBytes::new(b.data)),
        gas_used: 0,
    })
}

/// Execute the Burn action
fn burn(token: Address, amount: TokenAmount) -> u32 {
    let burn_params = BurnParams { amount };
    let ret = sdk::send::send(
        &token,
        method_hash!("Burn"),
        IpldBlock::serialize_cbor(&burn_params).unwrap(),
        TokenAmount::zero(),
    )
    .unwrap();
    if !ret.exit_code.is_success() {
        panic!("burn call failed");
    }
    NO_DATA_BLOCK_ID
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
                match action {
                    TestAction::Accept => {
                        // do nothing, return success
                        NO_DATA_BLOCK_ID
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
                        transfer(Address::new_id(sdk::message::caller()), to, token_params.amount, operator_data)
                    }
                    TestAction::Burn => {
                        // burn the tokens
                        burn(Address::new_id(sdk::message::caller()), token_params.amount)
                    }
                }
            },
            "Action" => {
                // take action independent of the receiver hook
                let params: ActionParams = deserialize_params(input);

                // get our balance
                let get_balance = || {
                    let self_address = Address::new_id(sdk::message::receiver());
    let balance_ret = sdk::send::send(&params.token_address, method_hash!("BalanceOf"), IpldBlock::serialize_cbor(&self_address).unwrap(), TokenAmount::zero()).unwrap();
                    if !balance_ret.exit_code.is_success() {
                        panic!("unable to get balance");
                    }
                    balance_ret.return_data.unwrap().deserialize::<TokenAmount>().unwrap()
                };

                match params.action {
                    TestAction::Accept | TestAction::Reject => {
                        sdk::vm::abort(
                            ExitCode::USR_ILLEGAL_ARGUMENT.value(),
                            Some("invalid argument"),
                        );
                    }
                    TestAction::Transfer(to, operator_data) => {
                        // transfer to a target address
                        let balance = get_balance();
                        transfer(params.token_address, to, balance, operator_data)
                    }
                    TestAction::Burn => {
                        // burn the tokens
                        let balance = get_balance();
                        burn(params.token_address, balance)
                    }
                }
            }
            _ => {
                sdk::vm::abort(
                    ExitCode::USR_UNHANDLED_MESSAGE.value(),
                    Some("Unknown method number"),
                );
            }
        })
}
