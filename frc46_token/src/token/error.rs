use fvm_actor_utils::messaging::MessagingError;
use fvm_actor_utils::receiver::ReceiverHookError;
use fvm_ipld_encoding::Error as SerializationError;
use fvm_shared::address::{Address, Error as AddressError};
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use thiserror::Error;

use crate::token::state::StateError as TokenStateError;
use crate::token::state::StateInvariantError;

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("error in underlying state {0}")]
    TokenState(#[from] TokenStateError),
    #[error("value {amount:?} for {name:?} must be non-negative")]
    InvalidNegative { name: &'static str, amount: TokenAmount },
    #[error("amount {amount:?} for {name:?} must be a multiple of {granularity:?}")]
    InvalidGranularity { name: &'static str, amount: TokenAmount, granularity: u64 },
    #[error("error calling other actor: {0}")]
    Messaging(#[from] MessagingError),
    #[error("receiver hook error: {0}")]
    ReceiverHook(#[from] ReceiverHookError),
    #[error("expected {address:?} to be a resolvable id address but threw {source:?} when attempting to resolve")]
    InvalidIdAddress {
        address: Address,
        #[source]
        source: AddressError,
    },
    #[error("operator cannot be the same as the debited address {0}")]
    InvalidOperator(Address),
    #[error("error during serialization {0}")]
    Serialization(#[from] SerializationError),
    #[error("error in state invariants {0}")]
    StateInvariant(#[from] StateInvariantError),
}

impl From<&TokenError> for ExitCode {
    fn from(error: &TokenError) -> Self {
        match error {
            TokenError::InvalidIdAddress { address: _, source: _ } => ExitCode::USR_NOT_FOUND,
            TokenError::Serialization(_) => ExitCode::USR_SERIALIZATION,
            TokenError::InvalidOperator(_)
            | TokenError::InvalidGranularity { name: _, amount: _, granularity: _ }
            | TokenError::InvalidNegative { name: _, amount: _ } => ExitCode::USR_ILLEGAL_ARGUMENT,
            TokenError::StateInvariant(_) => ExitCode::USR_ILLEGAL_STATE,
            TokenError::TokenState(state_error) => state_error.into(),
            TokenError::ReceiverHook(e) => e.into(),
            TokenError::Messaging(messaging_error) => messaging_error.into(),
        }
    }
}

#[cfg(test)]
mod test {
    use fvm_shared::error::ExitCode;

    use crate::token::TokenError;
    use crate::token::TokenStateError;

    #[test]
    fn it_creates_exit_codes() {
        let error = TokenError::TokenState(TokenStateError::MissingState(cid::Cid::default()));
        let msg = error.to_string();
        let exit_code = ExitCode::from(&error);
        // taking the exit code doesn't consume the error
        println!("{msg}: {exit_code:?}");
        assert_eq!(exit_code, ExitCode::USR_ILLEGAL_STATE);
    }
}
