use super::state::StateError;
use fvm_ipld_hamt::Error as HamtError;
use fvm_shared::address::Address;

pub enum ActorError {
    AddrNotFound(Address),
    IpldState(StateError),
    IpldHamt(HamtError),
    Arithmetic(String),
}

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
