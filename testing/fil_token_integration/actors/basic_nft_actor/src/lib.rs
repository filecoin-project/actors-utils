use frc42_dispatch::match_method;
use frc53_nft::{
    state::{NFTState, TokenID},
    types::{
        ApproveForAllParams, ApproveParams, BurnFromParams, ListOperatorTokensParams,
        ListOwnedTokensParams, ListTokenOperatorsParams, ListTokensParams, RevokeForAllParams,
        RevokeParams, TransferFromParams, TransferParams,
    },
    NFT,
};
use fvm_actor_utils::{
    blockstore::Blockstore, messaging::FvmMessenger, syscalls::fvm_syscalls::FvmSyscalls,
    util::ActorRuntime,
};
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
    let messenger = FvmMessenger::default();
    let root_cid = sdk::sself::root().unwrap();
    let helpers = ActorRuntime::<FvmSyscalls, Blockstore>::new_fvm_runtime();
    let mut state = NFTState::load(&helpers, &root_cid).unwrap();
    let mut handle = NFT::wrap(helpers, &mut state);

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
        "OwnerOf" => {
            let params = deserialize_params::<TokenID>(params);
            let res = handle.owner_of(params).unwrap();
            return_ipld(&res).unwrap()
        }
        "Metadata" => {
            let params = deserialize_params::<TokenID>(params);
            let res = handle.metadata(params).unwrap();
            return_ipld(&res).unwrap()
        }
        "Mint" => {
            let params = deserialize_params::<MintParams>(params);
            let caller = Address::new_id(sdk::message::caller());
            let mut hook = handle.mint(&caller, &params.initial_owner, params.metadata, params.operator_data, RawBytes::default()).unwrap();

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
                &params.from,
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
            let ret_val = handle.burn(&Address::new_id(caller), &params).unwrap();

            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            return_ipld(&ret_val).unwrap()
        }
        "BurnFrom" => {
            let params = deserialize_params::<BurnFromParams>(params);
            let caller = sdk::message::caller();
            handle.burn_from(&params.from, &Address::new_id(caller), &params.token_ids).unwrap();

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
        "ListTokens" => {
            let params = deserialize_params::<ListTokensParams>(params);
            let res = handle.list_tokens(params.cursor, params.max).unwrap();
            return_ipld(&res).unwrap()
        }
        "ListOwnedTokens" => {
            let params = deserialize_params::<ListOwnedTokensParams>(params);
            let res = handle.list_owned_tokens(&params.owner, params.cursor, params.max).unwrap();
            return_ipld(&res).unwrap()
        }
        "ListTokenOperators" => {
            let params = deserialize_params::<ListTokenOperatorsParams>(params);
            let res = handle.list_token_operators(&params.owner, params.token_id).unwrap();
            return_ipld(&res).unwrap()
        }
        "ListOperatorTokens" => {
            let params = deserialize_params::<ListOperatorTokensParams>(params);
            let res = handle.list_operator_tokens(&params.owner, &params.operator, params.cursor, params.max).unwrap();
            return_ipld(&res).unwrap()
        }
        "ListAccountOperators" => {
            let params = deserialize_params::<Address>(params);
            let res = handle.list_account_operators(&params).unwrap();
            return_ipld(&res).unwrap()
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
// testing/fil_token_integration/tests/frc53_nfts.rs::MintParams

/// Minting tokens goes directly to the caller for now
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    pub initial_owner: Address,
    pub metadata: Vec<String>,
    pub operator_data: RawBytes,
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().unwrap();
    let params = RawBytes::new(params.data);
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
