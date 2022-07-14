use fil_token::runtime::Runtime;
pub struct TestRuntime {
    caller: u64,
}

impl TestRuntime {
    pub fn new(caller: u64) -> Self {
        Self { caller }
    }
}

impl Runtime for TestRuntime {
    fn caller(&self) -> u64 {
        return self.caller;
    }

    fn resolve_address(&self, addr: &fvm_shared::address::Address) -> anyhow::Result<u64> {
        Ok(addr.id().unwrap())
    }
}
