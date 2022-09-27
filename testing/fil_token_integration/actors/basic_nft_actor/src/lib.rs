use frc42_dispatch::match_method;
use frcxx_nft::{
    state::{NFTState, TokenID},
    NFT,
};
use fvm_actor_utils::{blockstore::Blockstore, messaging::FvmMessenger};
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
    let root_cid = sdk::sself::root().unwrap();
    let mut state = NFTState::load(&bs, &root_cid).unwrap();
    let mut handle = NFT::wrap(bs, messenger, &mut state);

    match_method!(method_num,{
        "TotalSupply" => {
            let res = handle.total_supply();
            return_ipld(&res).unwrap()
        }
        "Mint" => {
            let params = deserialize_params::<MintParams>(params);
            let res = handle.mint(Address::new_id(sdk::message::caller()), params.metadata_id).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            return_ipld(&res).unwrap()
        }
        "Burn" => {
            let params = deserialize_params::<TokenID>(params);
            handle.burn(params).unwrap();
            let cid = handle.flush().unwrap();
            sdk::sself::set_root(&cid).unwrap();
            NO_DATA_BLOCK_ID
        }
        _ => {
            sdk::vm::abort(ExitCode::USR_ILLEGAL_ARGUMENT.value(), Some(&format!("Unknown method number {:?} was invoked", method_num)));
        }
    })
}

pub fn constructor() {
    let bs = Blockstore {};
    let nft_state = NFTState::new(&bs).unwrap();
    let state_cid = nft_state.save(&bs).unwrap();
    sdk::sself::set_root(&state_cid).unwrap();
}

/// Minting tokens goes directly to the caller for now
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    metadata_id: String,
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
