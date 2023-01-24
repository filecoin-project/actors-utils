use frc42_dispatch::method_hash;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_ipld_encoding::Error as IpldError;
use fvm_sdk::{send, sys::ErrorNumber};
use fvm_shared::error::ExitCode;
use fvm_shared::sys::SendFlags;
use fvm_shared::{address::Address, econ::TokenAmount};
use fvm_shared::{MethodNum, Response};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MessagingError>;

#[derive(Error, Debug)]
pub enum MessagingError {
    #[error("fvm syscall error: `{0}`")]
    Syscall(#[from] ErrorNumber),
    #[error("address could not be resolved: `{0}`")]
    AddressNotResolved(Address),
    #[error("address could not be initialized: `{0}`")]
    AddressNotInitialized(Address),
    #[error("ipld serialization error: `{0}`")]
    Ipld(#[from] IpldError),
}

impl From<&MessagingError> for ExitCode {
    fn from(error: &MessagingError) -> Self {
        match error {
            MessagingError::Syscall(e) => match e {
                ErrorNumber::IllegalArgument => ExitCode::USR_ILLEGAL_ARGUMENT,
                ErrorNumber::Forbidden | ErrorNumber::IllegalOperation => ExitCode::USR_FORBIDDEN,
                ErrorNumber::AssertionFailed => ExitCode::USR_ASSERTION_FAILED,
                ErrorNumber::InsufficientFunds => ExitCode::USR_INSUFFICIENT_FUNDS,
                ErrorNumber::IllegalCid | ErrorNumber::NotFound | ErrorNumber::InvalidHandle => {
                    ExitCode::USR_NOT_FOUND
                }
                ErrorNumber::Serialization | ErrorNumber::IllegalCodec => {
                    ExitCode::USR_SERIALIZATION
                }
                _ => ExitCode::USR_UNSPECIFIED,
            },
            MessagingError::AddressNotResolved(_) | MessagingError::AddressNotInitialized(_) => {
                ExitCode::USR_NOT_FOUND
            }
            MessagingError::Ipld(_) => ExitCode::USR_SERIALIZATION,
        }
    }
}

/// An abstraction used to send messages to other actors
pub trait Messaging {
    /// Sends a message to an actor
    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: Option<IpldBlock>,
        value: TokenAmount,
    ) -> Result<Response>;
}

/// This method number comes from taking the name as "Receive" and applying
/// the transformation described in [FRC-0042](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0042.md)
pub const RECEIVER_HOOK_METHOD_NUM: u64 = method_hash!("Receive");

#[derive(Debug, Default, Clone, Copy)]
pub struct FvmMessenger {}

impl Messaging for FvmMessenger {
    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: Option<IpldBlock>,
        value: TokenAmount,
    ) -> Result<Response> {
        Ok(send::send(to, method, params, value, None, SendFlags::empty())?)
    }
}
