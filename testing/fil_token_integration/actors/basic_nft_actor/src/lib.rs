use cid::Cid;
use frc42_dispatch::match_method;
use frcxx_nft::{
    state::{NFTState, TokenID},
    types::{
        ApproveForAllParams, ApproveParams, RevokeForAllParams, RevokeParams, TransferFromParams,
        TransferParams,
    },
    NFT,
};
use fvm_actor_utils::{actor::FvmActor, blockstore::Blockstore, messaging::FvmMessenger};
use fvm_ipld_encoding::{
    de::DeserializeOwned,
    ser,
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes, DAG_CBOR,
};
use fvm_sdk as sdk;
use fvm_shared::address::Address;
use fvm_shared::error::ExitCode;
use sdk::{sys::ErrorNumber, NO_DATA_BLOCK_ID};
use thiserror::Error;

#[no_mangle]
fn invoke(params: u32) -> u32 {
    let method_num = sdk::message::method_number();

    if method_num == 1 {
        constructor();
        return NO_DATA_BLOCK_ID;
    }

    // After constructor has run we have state
    let bs = Blockstore {};
    let messenger = FvmMessenger::default();
    let actor_helper = FvmActor {};
    let root_cid = sdk::sself::root().unwrap();
    let mut state = NFTState::load(&bs, &root_cid).unwrap();
    let mut handle = NFT::wrap(bs, messenger, actor_helper, &mut state);

    match_method!(method_num,{
        "BalanceOf" => {
            let params = deserialize_params::<Address>(params);
            let res = handle.balance_of(&params).unwrap();
            return_ipld(&res).unwrap()
        }
        "TotalSupply" => {
            let res = handle.total_supply();
            return_ipld(&res).unwrap()
        }
        "Mint" => {
            let params = deserialize_params::<MintParams>(params);
            let caller = Address::new_id(sdk::message::caller());
            let mut hook = handle.mint(&caller, &params.initial_owner, &params.metadata, params.operator_data, RawBytes::default()).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();

            let hook_res = hook.call(&messenger).unwrap();

            let ret_val = handle.mint_return(hook_res, cid).unwrap();
            return_ipld(&ret_val).unwrap()
        }
        "Transfer" => {
            let params = deserialize_params::<TransferParams>(params);
            let mut hook = handle.transfer(
                &caller_address(),
                &params.to,
                &params.token_ids,
                params.operator_data,
                RawBytes::default()
            ).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();

            let hook_res = hook.call(&messenger).unwrap();

            let ret_val = handle.transfer_return(hook_res, cid).unwrap();
            return_ipld(&ret_val).unwrap()
        }
        "TransferFrom" => {
            let params = deserialize_params::<TransferFromParams>(params);
            let mut hook = handle.transfer_from(
                &caller_address(),
                &params.to,
                &params.token_ids,
                params.operator_data,
                RawBytes::default()
            ).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();

            let hook_res = hook.call(&messenger).unwrap();

            let ret_val = handle.transfer_from_return(hook_res, cid).unwrap();
            return_ipld(&ret_val).unwrap()
        }
        "Burn" => {
            let params = deserialize_params::<Vec<TokenID>>(params);
            let caller = sdk::message::caller();
            let ret_val = handle.burn(caller, &params).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            return_ipld(&ret_val).unwrap()
        }
        "BurnFor" => {
            let params = deserialize_params::<Vec<TokenID>>(params);
            let caller = sdk::message::caller();
            handle.burn_for(caller, &params).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        "Approve" => {
            let params = deserialize_params::<ApproveParams>(params);
            handle.approve(&caller_address(), &params.operator, &params.token_ids).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        "Revoke" => {
            let params = deserialize_params::<RevokeParams>(params);
            handle.revoke(&caller_address(), &params.operator, &params.token_ids).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        "ApproveForAll" => {
            let params = deserialize_params::<ApproveForAllParams>(params);
            handle.approve_for_owner(&caller_address(), &params.operator).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        "RevokeForAll" => {
            let params = deserialize_params::<RevokeForAllParams>(params);
            handle.revoke_for_all(&caller_address(), &params.operator).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        _ => {
            sdk::vm::abort(ExitCode::USR_ILLEGAL_ARGUMENT.value(), Some(&format!("Unknown method number {method_num:?} was invoked")));
        }
    })
}

pub fn constructor() {
    let bs = Blockstore {};
    let nft_state = NFTState::new(&bs).unwrap();
    let state_cid = nft_state.save(&bs).unwrap();
    sdk::sself::set_root(&state_cid).unwrap();
}

// Note that the below MintParams needs to be manually synced with
// testing/fil_token_integration/tests/frcxx_nfts.rs::MintParams

/// Minting tokens goes directly to the caller for now
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    pub initial_owner: Address,
    pub metadata: Vec<Cid>,
    pub operator_data: RawBytes,
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
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

fn caller_address() -> Address {
    Address::new_id(sdk::message::caller())
}
