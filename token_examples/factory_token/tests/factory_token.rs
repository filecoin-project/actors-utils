use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::MintReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{econ::TokenAmount, receipt::Receipt};

mod common;
use common::{construct_tester, TestHelpers, TokenHelpers};

use factory_token::{token::BasicToken, ConstructorParams};

const FACTORY_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/factory_token/factory_token.compact.wasm";

#[test]
fn factory_token() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_token_state =
        BasicToken::new(&blockstore, String::new(), String::new(), 1);

    // install actors required for our test: a token actor and one instance of the test actor
    let token_actor =
        tester.install_actor_with_state(FACTORY_TOKEN_ACTOR_WASM, 10000, initial_token_state);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actor
    {
        let params =
            ConstructorParams { name: "Test Token".into(), symbol: "TEST".into(), granularity: 1 };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val =
            tester.call_method(operator[0].1, token_actor, method_hash!("Constructor"), Some(params));
        assert!(ret_val.msg_receipt.exit_code.is_success(), "token constructor returned {:#?}", ret_val);
    }
}
