use fil_fungible_token::runtime::blockstore::Blockstore;
use frcxx_nft::nft::state::NFTSetState;
use fvm_ipld_encoding::{de::DeserializeOwned, ser, RawBytes, DAG_CBOR};
use fvm_sdk as sdk;
use fvm_shared::error::ExitCode;
use sdk::NO_DATA_BLOCK_ID;

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
    match method_num {
        1 => {
            constructor();
            NO_DATA_BLOCK_ID
        }
        2 => {
            let bs = Blockstore {};
            let root_cid = sdk::sself::root().unwrap();
            let state = NFTSetState::load(&bs, &root_cid).unwrap();
            let res = state.mint_token(&bs, sdk::message::caller()).unwrap();
            return_ipld(&res)
        }
        _ => {
            sdk::vm::abort(ExitCode::USR_UNHANDLED_MESSAGE.value(), Some("Unknown method number"));
        }
    }
}

pub fn constructor() {
    let bs = Blockstore {};
    let nft_state = NFTSetState::new(&bs).unwrap();
    let state_cid = nft_state.save(&bs).unwrap();
    sdk::sself::set_root(&state_cid).unwrap();
}
