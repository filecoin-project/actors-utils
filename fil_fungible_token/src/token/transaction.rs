use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use cid::Cid;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use thiserror::Error;

use crate::method::MethodCaller;

use super::Result as TokenResult;
use super::{Token, TokenError};

#[derive(Error, Debug)]
pub enum TokenTransactionError {
    #[error("error in token operation {0}")]
    State(#[from] TokenError),
}

type Result<T> = std::result::Result<T, TokenTransactionError>;

pub enum TransactionOutcome {
    Succeeded(Cid),
    Reverted(Cid),
}

pub struct CleanTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Rc<RefCell<Token<BS, MC>>>,
    needs_rollback: bool,
}

pub struct DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Rc<RefCell<Token<BS, MC>>>,
    needs_rollback: bool,
}

impl<BS, MC> Clone for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn clone(&self) -> Self {
        DirtyTransaction { token: self.token.clone(), needs_rollback: self.needs_rollback }
    }
}

impl<BS, MC> From<CleanTransaction<BS, MC>> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn from(tx: CleanTransaction<BS, MC>) -> Self {
        Self { token: tx.token, needs_rollback: tx.needs_rollback }
    }
}

impl<BS, MC> From<&mut CleanTransaction<BS, MC>> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn from(tx: &mut CleanTransaction<BS, MC>) -> Self {
        Self { token: tx.token.clone(), needs_rollback: tx.needs_rollback }
    }
}

pub trait StateModifier<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC>;

    fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC>;

    fn revoke_allowance(&mut self, owner: ActorID, spender: ActorID) -> DirtyTransaction<BS, MC>;

    fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        amount: &TokenAmount,
    ) -> DirtyTransaction<BS, MC>;
}

pub trait Transaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn flush(&mut self) -> Result<TransactionOutcome>;
}

impl<BS, MC> CleanTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    pub fn new(bs: BS, mc: MC, token_cid: Cid) -> Result<Self> {
        let token = Token::load(bs, mc, token_cid)?;
        Ok(Self { token: Rc::new(RefCell::new(token)), needs_rollback: false })
    }

    fn apply_state_change<F, Res>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(RefMut<Token<BS, MC>>) -> TokenResult<Res>,
    {
        if self.needs_rollback {
            return self;
        }

        if let Err(_e) = f((*self.token).borrow_mut()) {
            // TODO: error logging, tracking or propagation of error up?
            self.needs_rollback = true;
        }

        self
    }

    pub fn mint_and_flush(
        &mut self,
        minter: ActorID,
        initial_holder: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> &mut CleanTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.mint(minter, initial_holder, value, data))
    }

    pub fn transfer_and_flush(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> &mut CleanTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.transfer(spender, owner, receiver, value, data))
    }
}

impl<BS, MC> StateModifier<BS, MC> for CleanTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.increase_allowance(owner, spender, delta));
        self.into()
    }

    fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.decrease_allowance(owner, spender, delta));
        self.into()
    }

    fn revoke_allowance(&mut self, owner: ActorID, spender: ActorID) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.revoke_allowance(owner, spender));
        self.into()
    }

    fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        amount: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.burn(spender, owner, amount));
        self.into()
    }
}

impl<BS, MC> DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn apply_state_change<F, Res>(&mut self, f: F) -> &mut Self
    where
        F: FnOnce(RefMut<Token<BS, MC>>) -> TokenResult<Res>,
    {
        if self.needs_rollback {
            return self;
        }

        if let Err(_e) = f((*self.token).borrow_mut()) {
            // TODO: error logging, tracking or propagation of error up?
            self.needs_rollback = true;
        }

        self
    }
}

impl<BS, MC> StateModifier<BS, MC> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.increase_allowance(owner, spender, delta));
        self.clone()
    }

    fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.decrease_allowance(owner, spender, delta));
        self.clone()
    }

    fn revoke_allowance(&mut self, owner: ActorID, spender: ActorID) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.revoke_allowance(owner, spender));
        self.clone()
    }

    fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        amount: &TokenAmount,
    ) -> DirtyTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.burn(spender, owner, amount));
        self.clone()
    }
}

impl<BS, MC> Transaction<BS, MC> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn flush(&mut self) -> Result<TransactionOutcome> {
        if self.needs_rollback {
            Ok(TransactionOutcome::Reverted(self.token.borrow_mut().revert()?))
        } else {
            Ok(TransactionOutcome::Succeeded(self.token.borrow_mut().flush()?))
        }
    }
}

#[cfg(test)]
mod test {
    use fvm_shared::{econ::TokenAmount, ActorID};
    use num_traits::Zero;

    const TOKEN_ACTOR_ADDRESS: ActorID = ActorID::max_value();
    const TREASURY: ActorID = 1;
    // const ALICE: ActorID = 2;
    // const BOB: ActorID = 3;

    use crate::{
        blockstore::SharedMemoryBlockstore,
        method::FakeMethodCaller,
        token::{
            transaction::{StateModifier, Transaction},
            Token,
        },
    };

    use super::{CleanTransaction, TransactionOutcome};

    fn new_transaction() -> CleanTransaction<SharedMemoryBlockstore, FakeMethodCaller> {
        let bs = SharedMemoryBlockstore::new();
        let (_token, cid) = Token::new(bs.clone(), FakeMethodCaller::default()).unwrap();

        CleanTransaction::new(bs, FakeMethodCaller::default(), cid).unwrap()
    }

    #[test]
    fn it_batches_changes() {
        let mut tx = new_transaction();

        let res = tx
            .mint_and_flush(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
            .burn(TREASURY, TREASURY, &TokenAmount::from(60))
            .flush()
            .unwrap();

        if let TransactionOutcome::Succeeded(_) = res {
            assert_eq!(tx.token.borrow().balance_of(TREASURY).unwrap(), TokenAmount::from(40));
        } else {
            panic!("expected success");
        }
    }

    #[test]
    fn it_fails_atomically() {
        let mut tx = new_transaction();

        // burn more than was minted
        let res = tx
            .mint_and_flush(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
            .burn(TREASURY, TREASURY, &TokenAmount::from(110))
            .flush()
            .unwrap();

        if let TransactionOutcome::Reverted(_) = res {
            assert_eq!(tx.token.borrow().balance_of(TREASURY).unwrap(), TokenAmount::zero());
        } else {
            panic!("expected revert");
        }
    }
}
