use cid::Cid;
use fvm_sdk as sdk;
use fvm_sdk::error::StateReadError;
use thiserror::Error;

#[derive(Error, Clone, Debug)]
pub enum ActorError {
    #[error("root state not found {0}")]
    NoState(#[from] StateReadError),
}

type Result<T> = std::result::Result<T, ActorError>;

/// Generic utils related to actors on the FVM
pub trait Actor {
    /// Get the root cid of the actor's state
    fn root_cid(&self) -> Result<Cid>;
}

/// A helper handle for actors deployed on FVM
pub struct FvmActor {}

impl Actor for FvmActor {
    fn root_cid(&self) -> Result<Cid> {
        Ok(sdk::sself::root()?)
    }
}

/// A fake actor fixture that can be twiddled for testing
#[derive(Default, Clone, Debug)]
pub struct FakeActor {
    pub root: Cid,
}

impl Actor for FakeActor {
    fn root_cid(&self) -> Result<Cid> {
        Ok(self.root)
    }
}
