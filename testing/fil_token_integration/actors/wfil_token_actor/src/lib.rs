use anyhow::Result;
use fil_token::blockstore::Blockstore;
use fil_token::runtime::FvmRuntime;
use fil_token::token::types::*;
use fil_token::token::{Token, TokenHelper};
use fvm_ipld_encoding::{RawBytes, DAG_CBOR};
use fvm_sdk as sdk;
use fvm_shared::bigint::bigint_ser::BigIntSer;
use fvm_shared::econ::TokenAmount;
use sdk::NO_DATA_BLOCK_ID;

struct WfilToken {
    /// Default token helper impl
    util: TokenHelper<Blockstore, FvmRuntime>,
}

// TODO: Wrapper is unecessary?
// Instead expose a
impl Token for WfilToken {
    fn name(&self) -> String {
        String::from("Wrapped FIL")
    }

    fn symbol(&self) -> String {
        String::from("WFIL")
    }

    fn total_supply(&self) -> TokenAmount {
        self.util.total_supply()
    }

    fn balance_of(
        &self,
        params: fvm_shared::address::Address,
    ) -> Result<fvm_shared::econ::TokenAmount> {
        self.util.balance_of(params)
    }

    fn increase_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn> {
        self.util.increase_allowance(params)
    }

    fn decrease_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn> {
        self.util.decrease_allowance(params)
    }

    fn revoke_allowance(&self, params: RevokeAllowanceParams) -> Result<AllowanceReturn> {
        self.util.revoke_allowance(params)
    }

    fn allowance(&self, params: GetAllowanceParams) -> Result<AllowanceReturn> {
        self.util.allowance(params)
    }

    fn burn(&self, params: BurnParams) -> Result<BurnReturn> {
        self.util.burn(params)
    }

    fn transfer(&self, params: TransferParams) -> Result<TransferReturn> {
        self.util.transfer(params)
    }
}

/// Placeholder invoke for testing
#[no_mangle]
pub fn invoke(params: u32) -> u32 {
    // Conduct method dispatch. Handle input parameters and return data.
    let method_num = sdk::message::method_number();

    let token_actor = WfilToken {
        util: TokenHelper::new(Blockstore {}, FvmRuntime {}),
    };

    //TODO: this internal dispatch can be pushed as a library function into the fil_token crate
    // - it should support a few different calling-conventions
    // - it should also handle deserialization of raw_params into the expected IPLD types
    let res = match method_num {
        // Actor constructor
        1 => constructor(),
        // Standard token interface
        2 => {
            token_actor.name();
            // TODO: store and return CID
            NO_DATA_BLOCK_ID
        }
        3 => {
            token_actor.symbol();
            // TODO: store and return CID
            NO_DATA_BLOCK_ID
        }
        4 => {
            token_actor.total_supply();
            // TODO: store and return CID
            NO_DATA_BLOCK_ID
        }
        5 => {
            // balance of
            let params = sdk::message::params_raw(params).unwrap().1;
            let params = RawBytes::new(params);
            let params = params.deserialize().unwrap();
            let res = token_actor.balance_of(params).unwrap();
            let res = RawBytes::new(fvm_ipld_encoding::to_vec(&BigIntSer(&res)).unwrap());
            let cid = sdk::ipld::put_block(DAG_CBOR, res.bytes()).unwrap();
            cid
        }
        6 => {
            // increase allowance
            NO_DATA_BLOCK_ID
        }
        7 => {
            // decrease allowance
            NO_DATA_BLOCK_ID
        }
        8 => {
            // revoke_allowance
            NO_DATA_BLOCK_ID
        }
        9 => {
            // allowance
            NO_DATA_BLOCK_ID
        }
        10 => {
            // burn
            NO_DATA_BLOCK_ID
        }
        11 => {
            // transfer_from
            NO_DATA_BLOCK_ID
        }
        // Custom actor interface
        12 => {
            // Mint
            let params = MintParams {
                initial_holder: sdk::message::caller(),
                value: sdk::message::value_received(),
            };
            let res = token_actor.util.mint(params).unwrap();
            let res = RawBytes::new(fvm_ipld_encoding::to_vec(&res).unwrap());
            let cid = sdk::ipld::put_block(DAG_CBOR, res.bytes()).unwrap();
            cid
        }
        _ => {
            sdk::vm::abort(
                fvm_shared::error::ExitCode::USR_ILLEGAL_ARGUMENT.value(),
                Some("Unknown method number"),
            );
        }
    };

    res
}

fn constructor() -> u32 {
    0_u32
}
