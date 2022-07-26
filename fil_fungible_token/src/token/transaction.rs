use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use thiserror::Error;

use crate::method::MethodCaller;

use super::{Token, TokenError};

#[derive(Error, Debug)]
pub enum TokenTransactionError {
    #[error("error in token operation {0}")]
    State(#[from] TokenError),
}

type Result<T> = std::result::Result<T, TokenTransactionError>;

pub struct StateTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: Token<BS, MC>,

    token_snapshot: Option<Token<BS, MC>>,
}

impl<BS, MC> StateTransaction<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    pub fn new(token: Token<BS, MC>) -> Self {
        Self { token, token_snapshot: None }
    }

    pub fn start_transaction(&self) -> {
        self.token_snapshot = Some(self.token.clone());
    }
}

pub trait StateReadable<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn total_supply(&self) -> TokenAmount;

    fn balance_of(&self, holder: ActorID) -> Result<TokenAmount>;

    fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount>;

    fn flush(&'tok mut self) -> Result<CleanStateTransaction<'tok, BS, MC>>;

    fn revert(&'tok mut self) -> Result<CleanStateTransaction<'tok, BS, MC>>;
}

pub trait StateClean<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn mint(
        &mut self,
        minter: ActorID,
        initial_holder: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<DirtyStateTransaction<BS, MC>>;

    fn transfer(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<DirtyStateTransaction<BS, MC>>;
}

pub struct CleanStateTransaction<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: &'tok mut Token<BS, MC>,
    token_snapshot: &'tok Token<BS, MC>
}

impl<'tok, BS, MC> CleanStateTransaction<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn new(token: &'tok mut Token<BS, MC>, token_snapshot: &'tok mut Token<BS, MC>) -> Self {
        Self { token, token_snapshot }
    }

    pub fn mint(
        &mut self,
        minter: ActorID,
        initial_holder: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        match self.token.mint(minter, initial_holder, value, data) {
            Ok(_) => Ok(DirtyStateTransaction::new(self.token, self.token_snapshot)) ,
            Err(_) => {
        }
    }

    pub fn transfer(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.transfer(spender, owner, receiver, value, data)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }

    pub fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.increase_allowance(owner, spender, delta)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }

    pub fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.decrease_allowance(owner, spender, delta)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }

    pub fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        value: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.burn(spender, owner, value)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }
}

impl<'tok, BS, MC> StateReadable<'tok, BS, MC> for CleanStateTransaction<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn total_supply(&self) -> TokenAmount {
        self.token.total_supply()
    }

    fn balance_of(&self, holder: ActorID) -> Result<TokenAmount> {
        Ok(self.token.balance_of(holder)?)
    }

    fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount> {
        Ok(self.token.allowance(owner, spender)?)
    }

    fn flush(&'tok mut self) -> Result<CleanStateTransaction<'tok, BS, MC>> {
        self.token.flush()?;
        Ok(CleanStateTransaction::new(self.token))
    }
}

#[derive(Debug)]
pub struct DirtyStateTransaction<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    token: &'tok mut Token<BS, MC>,
    token_snapshot: &'tok Token<BS, MC>
}

impl<'tok, BS, MC> DirtyStateTransaction<'tok, BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    fn new(token: &'tok mut Token<BS, MC>, snapshot: &'tok Token<BS, MC>) -> Self {
        Self { token, token_snapshot: token }
    }

    pub fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.increase_allowance(owner, spender, delta)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }

    pub fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.decrease_allowance(owner, spender, delta)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }

    pub fn burn(
        &mut self,
        spender: ActorID,
        owner: ActorID,
        value: &TokenAmount,
    ) -> Result<DirtyStateTransaction<BS, MC>> {
        self.token.burn(spender, owner, value)?;
        Ok(DirtyStateTransaction::new(self.token, self.token_snapshot))
    }
}

#[cfg(test)]
mod test {
    use fvm_shared::{econ::TokenAmount, ActorID};

    use crate::{blockstore::SharedMemoryBlockstore, method::FakeMethodCaller, token::Token};

    use super::{CleanStateTransaction, StateReadable};
    const TOKEN_ACTOR_ADDRESS: ActorID = ActorID::max_value();
    const TREASURY: ActorID = 1;
    const ALICE: ActorID = 2;
    const BOB: ActorID = 3;

    fn new_token() -> Token<SharedMemoryBlockstore, FakeMethodCaller> {
        Token::new(SharedMemoryBlockstore::new(), FakeMethodCaller::default(), TOKEN_ACTOR_ADDRESS)
            .unwrap()
            .0
    }

    #[test]
    fn it_batches_changes() {
        let mut token = new_token();
        let mut state = CleanStateTransaction::new(&mut token);
        state
            .mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
            .unwrap()
            .burn(TREASURY, TREASURY, &TokenAmount::from(60))
            .unwrap();

        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(40));
    }

    #[test]
    fn it_fails_atomically() {
        let mut token = new_token();
        let mut state = CleanStateTransaction::new(&mut token);
        state
            .mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[])
            .unwrap()
            .burn(TREASURY, TREASURY, &TokenAmount::from(-200))
            .unwrap_err();

        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(40));
    }
}
