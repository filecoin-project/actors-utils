use std::{error::Error, fmt::Display};

use fvm_ipld_hamt::Error as HamtError;
use fvm_shared::address::Address;

#[derive(Debug)]
pub enum RuntimeError {
    AddrNotFound(Address),
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::AddrNotFound(_) => write!(f, "Address not found: {}", self),
        }
    }
}

impl Error for RuntimeError {}

#[derive(Debug)]
pub enum StateError {}

impl Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "State error")
    }
}

impl Error for StateError {}

#[derive(Debug)]
pub enum ActorError {
    AddrNotFound(Address),
    Arithmetic(String),
    IpldState(StateError),
    IpldHamt(HamtError),
    RuntimeError(RuntimeError),
}

impl Display for ActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorError::AddrNotFound(e) => write!(f, "{}", e),
            ActorError::Arithmetic(e) => write!(f, "{}", e),
            ActorError::IpldState(e) => write!(f, "{}", e),
            ActorError::IpldHamt(e) => write!(f, "{}", e),
            ActorError::RuntimeError(e) => write!(f, "{}", e),
        }
    }
}

impl Error for ActorError {}

impl From<StateError> for ActorError {
    fn from(e: StateError) -> Self {
        Self::IpldState(e)
    }
}

impl From<HamtError> for ActorError {
    fn from(e: HamtError) -> Self {
        Self::IpldHamt(e)
    }
}

impl From<RuntimeError> for ActorError {
    fn from(e: RuntimeError) -> Self {
        ActorError::RuntimeError(e)
    }
}
