use fil_fungible_token::runtime::blockstore::Blockstore;
use frc42_dispatch::match_method;
use frcxx_nft::{
    nft::state::{BatchMintReturn, NFTState},
    NFT,
};
use fvm_ipld_encoding::{
    de::DeserializeOwned,
    repr::Deserialize_repr,
    ser,
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes, DAG_CBOR,
};
use fvm_sdk as sdk;
use fvm_shared::error::ExitCode;
use sdk::NO_DATA_BLOCK_ID;

/// Minting tokens goes directly to the caller for now
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
struct MintParams {
    metadata_uri: String,
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}

fn return_ipld<T>(value: &T) -> u32
where
    T: ser::Serialize + ?Sized,
{
    let bytes = fvm_ipld_encoding::to_vec(value).unwrap();
    sdk::ipld::put_block(DAG_CBOR, bytes.as_slice()).unwrap()
}

#[no_mangle]
fn invoke(_input: u32) -> u32 {
    let method_num = sdk::message::method_number();

    if method_num == 1 {
        return constructor();
    }

    // After constructor has run we have state
    let bs: Blockstore = Blockstore {};
    let root_cid = sdk::sself::root().unwrap();
    let state = NFTState::load(&bs, &root_cid).unwrap();
    let handle = NFT::wrap(bs, state);

    match method_num {
        3839021839 => {
            /// Mint
            
        }
        _ => {
            sdk::vm::abort(ExitCode::USR_ILLEGAL_ARGUMENT.value(), Some("Unknown method number"));
        }
    }
}

pub fn constructor() {
    let bs = Blockstore {};
    let nft_state = NFTState::new(&bs).unwrap();
    let state_cid = nft_state.save(&bs).unwrap();
    sdk::sself::set_root(&state_cid).unwrap();
    NO_DATA_BLOCK_ID
}
