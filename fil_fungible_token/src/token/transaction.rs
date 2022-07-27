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

pub enum TransactionResult<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    Succeeded(Rc<Token<BS, MC>>),
    Reverted(Rc<Token<BS, MC>>),
}

pub struct CleanTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Rc<RefCell<Token<BS, MC>>>,
    token_snapshot: Rc<Token<BS, MC>>,
    needs_rollback: bool,
}

pub struct DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Rc<RefCell<Token<BS, MC>>>,
    token_snapshot: Rc<Token<BS, MC>>,
    needs_rollback: bool,
}

impl<BS, MC> Clone for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn clone(&self) -> Self {
        DirtyTransaction {
            token: self.token.clone(),
            token_snapshot: self.token_snapshot.clone(),
            needs_rollback: self.needs_rollback,
        }
    }
}

impl<BS, MC> From<CleanTransaction<BS, MC>> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn from(tx: CleanTransaction<BS, MC>) -> Self {
        Self {
            token: tx.token.clone(),
            token_snapshot: tx.token_snapshot.clone(),
            needs_rollback: tx.needs_rollback,
        }
    }
}

impl<BS, MC> From<&mut CleanTransaction<BS, MC>> for DirtyTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn from(tx: &mut CleanTransaction<BS, MC>) -> Self {
        Self {
            token: tx.token.clone(),
            token_snapshot: tx.token_snapshot.clone(),
            needs_rollback: tx.needs_rollback,
        }
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
    fn flush(&mut self) -> Result<Cid>;
}

impl<BS, MC> CleanTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    // FIXME: Transaction should probably hold a TokenState not a Token
    // fn new(token: Token<BS, MC>) -> Self {
    //     Self {
    //         token: Rc::new(RefCell::new(token)),
    //         token_snapshot: Rc::new(token),
    //         needs_rollback: false,
    //     }
    // }

    fn apply_state_change<F, Res>(&mut self, f: F) -> &Self
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
    ) -> &CleanTransaction<BS, MC> {
        self.apply_state_change(|mut token| token.mint(minter, initial_holder, value, data))
    }

    pub fn transfer_and_flush(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> &CleanTransaction<BS, MC> {
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
    fn apply_state_change<F, Res>(&mut self, f: F) -> &Self
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
    fn flush(&mut self) -> Result<Cid> {
        if self.needs_rollback {
            // TODO handle this
            panic!()
        } else {
            Ok((*self.token).borrow_mut().flush()?)
        }
    }
}

#[cfg(test)]
mod test {
    // use fvm_shared::{econ::TokenAmount, ActorID};

    // use crate::{blockstore::SharedMemoryBlockstore, method::FakeMethodCaller, token::Token};

    // const TOKEN_ACTOR_ADDRESS: ActorID = ActorID::max_value();
    // const TREASURY: ActorID = 1;
    // const ALICE: ActorID = 2;
    // const BOB: ActorID = 3;

    // fn new_transaction() -> Token<SharedMemoryBlockstore, FakeMethodCaller> {
    //     Token::new(SharedMemoryBlockstore::new(), FakeMethodCaller::default(), TOKEN_ACTOR_ADDRESS)
    //         .unwrap()
    //         .0
    // }

    // #[test]
    // fn it_batches_changes() {
    //     let mut token = new_token();
    //     let mut state = CleanStateTransaction::new(&mut token);
    //     state
    //         .mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
    //         .unwrap()
    //         .burn(TREASURY, TREASURY, &TokenAmount::from(60))
    //         .unwrap();

    //     assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(40));
    // }

    // #[test]
    // fn it_fails_atomically() {
    //     let mut token = new_token();
    //     let mut state = CleanStateTransaction::new(&mut token);
    //     state
    //         .mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
    //         .unwrap()
    //         .burn(TREASURY, TREASURY, &TokenAmount::from(-200))
    //         .unwrap_err();

    //     assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(40));
    // }
}
