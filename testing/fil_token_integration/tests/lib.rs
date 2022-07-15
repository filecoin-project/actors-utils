use std::env;

use fil_token::blockstore::Blockstore as ActorBlockstore;
use fvm_integration_tests::tester::{Account, Tester};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;

const WFIL_TOKEN_WASM_COMPILED_PATH: &str =
    "../../target/debug/wbuild/wfil_token_actor/wfil_token_actor.compact.wasm";

#[test]
fn mint_tokens() {
    let mut tester = Tester::new(
        NetworkVersion::V15,
        StateTreeVersion::V4,
        MemoryBlockstore::default(),
    )
    .unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // // Get wasm bin
    // let wasm_path = env::current_dir()
    //     .unwrap()
    //     .join(WFIL_TOKEN_WASM_COMPILED_PATH)
    //     .canonicalize()
    //     .unwrap();
    // let wasm_bin = std::fs::read(wasm_path).expect("Unable to read file");

    // let actor_blockstore = ActorBlockstore::default();
    // let actor_state = TokenState::new(&actor_blockstore).unwrap();
    // let state_cid = tester.set_state(&actor_state).unwrap();

    // let actor_address = Address::new_id(10000);
    // tester.set_actor_from_bin(&wasm_bin, state_cid, actor_address, TokenAmount::zero());

    // // Instantiate machine
    // tester.instantiate_machine().unwrap();

    // let message = Message {
    //     from: minter[0].1,
    //     to: actor_address,
    //     gas_limit: 10000000,
    //     method_num: 12, // 12 is Mint function
    //     value: TokenAmount::from(150),
    //     ..Message::default()
    // };

    // let res = tester
    //     .executor
    //     .unwrap()
    //     .execute_message(message, ApplyKind::Explicit, 100)
    //     .unwrap();
}
