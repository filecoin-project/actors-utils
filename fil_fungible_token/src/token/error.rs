use fvm_ipld_encoding::Error as SerializationError;
use fvm_sdk::sys::ErrorNumber;
use fvm_shared::address::{Address, Error as AddressError};
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::ActorID;
use thiserror::Error;

use super::{BurnResult, TransferResult};
use crate::runtime::messaging::MessagingError;
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
    #[error("error calling receiver hook: {0}")]
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
    #[error("expected {address:?} to be a resolvable id address but threw {source:?} when attempting to resolve")]
    InvalidIdAddress {
        address: Address,
        #[source]
        source: AddressError,
    },
    #[error("error during serialization {0}")]
    Serialization(#[from] SerializationError),
    #[error("error in state invariants {0}")]
    StateInvariant(#[from] StateInvariantError),
    #[error("unexpected transfer result type {result:?}")]
    TransferReturn { result: TransferResult },
    #[error("unexpected burn result type {result:?}")]
    BurnReturn { result: BurnResult },
}

impl From<TokenError> for ExitCode {
    fn from(error: TokenError) -> Self {
        match error {
            TokenError::ReceiverHook { from: _, to: _, operator: _, amount: _, exit_code } => {
                // simply pass through the exit code from the receiver but we could set a flag bit to
                // distinguish it if needed (e.g. 0x0100 | exit_code)
                exit_code
            }
            TokenError::InvalidIdAddress { address: _, source: _ } => ExitCode::USR_NOT_FOUND,
            TokenError::Serialization(_) => ExitCode::USR_SERIALIZATION,
            TokenError::InvalidGranularity { name: _, amount: _, granularity: _ }
            | TokenError::InvalidNegative { name: _, amount: _ } => ExitCode::USR_ILLEGAL_ARGUMENT,
            TokenError::TransferReturn { result: _ }
            | TokenError::BurnReturn { result: _ }
            | TokenError::StateInvariant(_) => ExitCode::USR_ILLEGAL_STATE,
            TokenError::TokenState(state_error) => match state_error {
                TokenStateError::IpldHamt(_) | TokenStateError::Serialization(_) => {
                    ExitCode::USR_SERIALIZATION
                }
                TokenStateError::NegativeTotalSupply { supply: _, delta: _ }
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
