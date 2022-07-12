mod blockstore;
mod runtime;
use blockstore::MemoryBlockstore;
use runtime::TestRuntime;

use fil_token::token::{Token, TokenHelper};

#[test]
fn it_mints() {
    let token = TokenHelper::new(MemoryBlockstore::new(), TestRuntime::new(1));
}
