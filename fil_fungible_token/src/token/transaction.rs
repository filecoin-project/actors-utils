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
    #[error("attempted to read from dirty state. call `TokenTransaction::flush` first")]
    ReadDirty,
}

type Result<T> = std::result::Result<T, TokenTransactionError>;

pub enum TransactionOutcome {
    Succeeded(Cid),
    Reverted(Cid),
}

pub struct TokenTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Rc<RefCell<Token<BS, MC>>>,
    needs_rollback: bool,
    state_dirty: bool,
}

impl<BS, MC> TokenTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    pub fn new(bs: BS, mc: MC) -> Result<Self> {
        let token = Token::new(bs, mc)?;
        Ok(Self {
            needs_rollback: false,
            state_dirty: false,
            token: Rc::new(RefCell::new(token.0)),
        })
    }

    pub fn load(bs: BS, mc: MC, token_cid: Cid) -> Result<Self> {
        let token = Token::load(bs, mc, token_cid)?;
        Ok(Self { needs_rollback: false, state_dirty: false, token: Rc::new(RefCell::new(token)) })
    }

    pub fn flush(&mut self) -> Result<TransactionOutcome> {
        if self.state_dirty && self.needs_rollback {
            let cid = self.token.borrow_mut().revert()?;
            self.state_dirty = false;
            self.needs_rollback = false;
            Ok(TransactionOutcome::Reverted(cid))
        } else {
            let cid = self.token.borrow_mut().flush()?;
            self.state_dirty = false;
            self.needs_rollback = false;
            Ok(TransactionOutcome::Succeeded(cid))
        }
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
}

impl<BS, MC> TokenTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    /// Gets the total number of tokens in existence
    ///
    /// This equals the sum of `balance_of` called on all addresses. This equals sum of all
    /// successful `mint` calls minus the sum of all successful `burn`/`burn_from` calls
    pub fn total_supply(&self) -> Result<TokenAmount> {
        if self.state_dirty {
            return Err(TokenTransactionError::ReadDirty);
        }
        Ok(RefCell::borrow(&self.token).total_supply())
    }

    /// Returns the balance associated with a particular address
    ///
    /// Accounts that have never received transfers implicitly have a zero-balance
    pub fn balance_of(&self, holder: ActorID) -> Result<TokenAmount> {
        if self.state_dirty {
            return Err(TokenTransactionError::ReadDirty);
        }
        Ok(RefCell::borrow(&self.token).balance_of(holder)?)
    }

    /// Gets the allowance between owner and spender
    ///
    /// The allowance is the amount that the spender can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount> {
        if self.state_dirty {
            return Err(TokenTransactionError::ReadDirty);
        }
        Ok(RefCell::borrow(&self.token).allowance(owner, spender)?)
    }

    pub fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> &mut TokenTransaction<BS, MC> {
        self.state_dirty = true;
        self.apply_state_change(|mut token| token.increase_allowance(owner, spender, delta))
    }

    pub fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> &mut TokenTransaction<BS, MC> {
        self.state_dirty = true;
        self.apply_state_change(|mut token| token.decrease_allowance(owner, spender, delta))
    }

    pub fn revoke_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
    ) -> &mut TokenTransaction<BS, MC> {
        self.state_dirty = true;
        self.apply_state_change(|mut token| token.revoke_allowance(owner, spender))
    }

    pub fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        amount: &TokenAmount,
    ) -> &mut TokenTransaction<BS, MC> {
        self.state_dirty = true;
        self.apply_state_change(|mut token| token.burn(spender, owner, amount))
    }

    pub fn mint(
        &mut self,
        minter: ActorID,
        initial_holder: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> &mut TokenTransaction<BS, MC> {
        if self.state_dirty {
            self.needs_rollback = true;
            return self;
        }
        self.apply_state_change(|mut token| token.mint(minter, initial_holder, value, data))
    }

    pub fn transfer(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> &mut TokenTransaction<BS, MC> {
        if self.state_dirty {
            self.needs_rollback = true;
            return self;
        }
        self.apply_state_change(|mut token| token.transfer(spender, owner, receiver, value, data))
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

    use crate::{blockstore::SharedMemoryBlockstore, method::FakeMethodCaller, token::Token};

    use super::{TokenTransaction, TransactionOutcome};

    fn new_transaction() -> TokenTransaction<SharedMemoryBlockstore, FakeMethodCaller> {
        let bs = SharedMemoryBlockstore::new();
        let (_token, cid) = Token::new(bs.clone(), FakeMethodCaller::default()).unwrap();

        TokenTransaction::new(token).unwrap()
    }

    // #[test]
    // fn it_batches_changes() {
    //     let mut tx = new_transaction();

    //     let res = tx
    //         .mint_and_flush(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
    //         .burn(TREASURY, TREASURY, &TokenAmount::from(60))
    //         .flush()
    //         .unwrap();

    //     if let TransactionOutcome::Succeeded(_) = res {
    //         assert_eq!(tx.token.borrow().balance_of(TREASURY).unwrap(), TokenAmount::from(40));
    //     } else {
    //         panic!("expected success");
    //     }
    // }

    // #[test]
    // fn it_fails_atomically() {
    //     let mut tx = new_transaction();

    //     // burn more than was minted
    //     let res = tx
    //         .mint_and_flush(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
    //         .burn(TREASURY, TREASURY, &TokenAmount::from(110))
    //         .flush()
    //         .unwrap();

    //     if let TransactionOutcome::Reverted(_) = res {
    //         assert_eq!(tx.token.borrow().balance_of(TREASURY).unwrap(), TokenAmount::zero());
    //     } else {
    //         panic!("expected revert");
    //     }
    // }
}
