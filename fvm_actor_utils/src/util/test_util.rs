use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::receipt::Receipt;
use fvm_shared::MethodNum;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};

use super::Actor;
use super::ActorResult;
use super::MessagingResult;
use crate::actor::Actor as OldActor;
use crate::actor::FakeActor;
use crate::messaging::FakeMessenger;
use crate::messaging::Messaging;

pub struct TestActor {
    pub bs: MemoryBlockstore,
    pub messenger: FakeMessenger,
    pub actor: FakeActor,
}

impl Actor for TestActor {
    fn actor_id(&self) -> ActorID {
        self.messenger.actor_id()
    }

    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: RawBytes,
        value: TokenAmount,
    ) -> MessagingResult<Receipt> {
        // FIXME: Hacky unwrap until we deduplicate the util::MessagingResult from messaging::MessagingResult
        //  Do not merge this
        Ok(self.messenger.send(to, method, &params, &value).unwrap())
    }

    fn resolve_id(&self, address: &Address) -> MessagingResult<ActorID> {
        // FIXME: Hacky unwrap until we deduplicate the util::MessagingResult from messaging::MessagingResult
        //  Do not merge this
        Ok(self.messenger.resolve_id(address).unwrap())
    }

    fn initialize_account(&self, address: &Address) -> MessagingResult<ActorID> {
        // FIXME: Hacky unwrap until we deduplicate the util::MessagingResult from messaging::MessagingResult
        //  Do not merge this
        Ok(self.messenger.initialize_account(address).unwrap())
    }

    fn root_cid(&self) -> ActorResult<Cid> {
        // FIXME: Hacky unwrap until we deduplicate the util::ActorResult from actor::ActorResult
        //  Do not merge this
        Ok(self.actor.root_cid().unwrap())
    }
}

/// Use an in-memory blockstore for testing
impl Blockstore for TestActor {
    fn get(&self, k: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        self.bs.get(k)
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> anyhow::Result<()> {
        self.bs.put_keyed(k, block)
    }
}
