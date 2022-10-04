use fvm_actor_utils::messaging::MessagingError;
use fvm_ipld_encoding::Error as SerializationError;
use fvm_sdk::sys::ErrorNumber;
use fvm_shared::address::{Address, Error as AddressError};
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::ActorID;
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
    #[error("receiver hook aborted when {operator:?} sent {amount:?} to {to:?} from {from:?} with exit code {exit_code:?}")]
    ReceiverHook {
        /// Whose balance is being debited
        from: ActorID,
        /// Whose balance is being credited
        to: ActorID,
        /// Who initiated the transfer of funds
        operator: ActorID,
        amount: TokenAmount,
        exit_code: ExitCode,
    },
    #[error("receiver hook was called already")]
    ReceiverHookAlreadyCalled,
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
            TokenError::ReceiverHook { from: _, to: _, operator: _, amount: _, exit_code } => {
                // simply pass through the exit code from the receiver but we could set a flag bit to
                // distinguish it if needed (e.g. 0x0100 | exit_code)
                *exit_code
            }
            TokenError::ReceiverHookAlreadyCalled => ExitCode::USR_ASSERTION_FAILED,
            TokenError::InvalidIdAddress { address: _, source: _ } => ExitCode::USR_NOT_FOUND,
            TokenError::Serialization(_) => ExitCode::USR_SERIALIZATION,
            TokenError::InvalidOperator(_)
            | TokenError::InvalidGranularity { name: _, amount: _, granularity: _ }
            | TokenError::InvalidNegative { name: _, amount: _ } => ExitCode::USR_ILLEGAL_ARGUMENT,
            TokenError::StateInvariant(_) => ExitCode::USR_ILLEGAL_STATE,
            TokenError::TokenState(state_error) => match state_error {
                TokenStateError::IpldHamt(_) | TokenStateError::Serialization(_) => {
                    ExitCode::USR_SERIALIZATION
                }
                TokenStateError::NegativeBalance { amount: _, owner: _ }
                | TokenStateError::NegativeAllowance { amount: _, owner: _, operator: _ }
                | TokenStateError::NegativeTotalSupply { supply: _, delta: _ }
                | TokenStateError::MissingState(_) => ExitCode::USR_ILLEGAL_STATE,
                TokenStateError::InsufficientBalance { balance: _, delta: _, owner: _ }
                | TokenStateError::InsufficientAllowance {
                    owner: _,
                    operator: _,
                    allowance: _,
                    delta: _,
                } => ExitCode::USR_INSUFFICIENT_FUNDS,
            },
            TokenError::Messaging(messaging_error) => match messaging_error {
                MessagingError::Syscall(e) => match e {
                    ErrorNumber::IllegalArgument => ExitCode::USR_ILLEGAL_ARGUMENT,
                    ErrorNumber::Forbidden | ErrorNumber::IllegalOperation => {
                        ExitCode::USR_FORBIDDEN
                    }
                    ErrorNumber::AssertionFailed => ExitCode::USR_ASSERTION_FAILED,
                    ErrorNumber::InsufficientFunds => ExitCode::USR_INSUFFICIENT_FUNDS,
                    ErrorNumber::IllegalCid
                    | ErrorNumber::NotFound
                    | ErrorNumber::InvalidHandle => ExitCode::USR_NOT_FOUND,
                    ErrorNumber::Serialization | ErrorNumber::IllegalCodec => {
                        ExitCode::USR_SERIALIZATION
                    }
                    ErrorNumber::LimitExceeded => ExitCode::USR_UNSPECIFIED,
                    _ => unreachable!(),
                },
                MessagingError::AddressNotResolved(_)
                | MessagingError::AddressNotInitialized(_) => ExitCode::USR_NOT_FOUND,
                MessagingError::Ipld(_) => ExitCode::USR_SERIALIZATION,
            },
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
        println!("{}: {:?}", msg, exit_code);
        assert_eq!(exit_code, ExitCode::USR_ILLEGAL_STATE);
    }
}
