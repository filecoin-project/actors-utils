use std::mem;

use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use num_traits::Zero;
use thiserror::Error;

use crate::messaging::{Messaging, MessagingError, RECEIVER_HOOK_METHOD_NUM};

/// Parameters for universal receiver
///
/// Actual payload varies with asset type
/// eg: FRC46_TOKEN_TYPE will come with a payload of FRC46TokenReceived
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct UniversalReceiverParams {
    /// Asset type
    pub type_: ReceiverType,
    /// Payload corresponding to asset type
    pub payload: RawBytes,
}

/// Standard interface for an actor that wishes to receive FRC-0046 tokens or other assets
pub trait UniversalReceiver {
    /// Invoked by a token actor during pending transfer or mint to the receiver's address
    ///
    /// Within this hook, the token actor has optimistically persisted the new balance so
    /// the receiving actor can immediately utilise the received funds. If the receiver wishes to
    /// reject the incoming transfer, this function should abort which will cause the token actor
    /// to rollback the transaction.
    fn receive(params: UniversalReceiverParams);
}

/// Type of asset received - could be tokens (FRC46 or other) or other assets
pub type ReceiverType = u32;

#[derive(Error, Debug)]
pub enum ReceiverHookError {
    #[error("receiver hook was not called")]
    NotCalled,
    #[error("receiver hook was already called")]
    AlreadyCalled,
    #[error("error encoding to ipld")]
    IpldEncoding(#[from] fvm_ipld_encoding::Error),
    #[error("error sending message")]
    Messaging(#[from] MessagingError),
    #[error("receiver hook error from {address:?}: exit_code={exit_code:?}, return_data={return_data:?}")]
    Receiver { address: Address, exit_code: ExitCode, return_data: RawBytes },
}

impl From<&ReceiverHookError> for ExitCode {
    fn from(error: &ReceiverHookError) -> Self {
        match error {
            ReceiverHookError::NotCalled | ReceiverHookError::AlreadyCalled => {
                ExitCode::USR_ASSERTION_FAILED
            }
            ReceiverHookError::IpldEncoding(_) => ExitCode::USR_SERIALIZATION,
            ReceiverHookError::Receiver { address: _, return_data: _, exit_code } => *exit_code,
            ReceiverHookError::Messaging(e) => e.into(),
        }
    }
}

pub trait RecipientData {
    fn set_recipient_data(&mut self, data: RawBytes);
}

/// Implements a guarded call to a token receiver hook
///
/// Mint and Transfer operations will return this so that state can be updated and saved
/// before making the call into the receiver hook.
///
/// This also tracks whether the call has been made or not, and
/// will panic if dropped without calling the hook.
#[derive(Debug)]
pub struct ReceiverHook<T: RecipientData> {
    address: Address,
    token_type: ReceiverType,
    token_params: RawBytes,
    called: bool,
    result_data: Option<T>,
}

impl<T: RecipientData> ReceiverHook<T> {
    /// Construct a new ReceiverHook call
    pub fn new(
        address: Address,
        token_params: RawBytes,
        token_type: ReceiverType,
        result_data: T,
    ) -> Self {
        ReceiverHook {
            address,
            token_params,
            token_type,
            called: false,
            result_data: Some(result_data),
        }
    }

    /// Call the receiver hook and return the result
    ///
    /// Requires the same Messaging trait as the Token
    /// eg: `hook.call(token.msg())?;`
    ///
    /// Returns
    /// - an error if already called
    /// - an error if the hook call aborted
    /// - any return data provided by the hook upon success
    pub fn call(&mut self, msg: &dyn Messaging) -> std::result::Result<T, ReceiverHookError> {
        if self.called {
            return Err(ReceiverHookError::AlreadyCalled);
        }

        self.called = true;

        let params = UniversalReceiverParams {
            type_: self.token_type,
            payload: mem::take(&mut self.token_params), // once encoded and sent, we don't need this anymore
        };

        let receipt = match msg.send(
            &self.address,
            RECEIVER_HOOK_METHOD_NUM,
            IpldBlock::serialize_cbor(&params).map_err(|e| {
                ReceiverHookError::IpldEncoding(fvm_ipld_encoding::Error {
                    description: e.to_string(),
                    protocol: fvm_ipld_encoding::CodecProtocol::Cbor,
                })
            })?,
            &TokenAmount::zero(),
        ) {
            Ok(receipt) => receipt,
            Err(e) => return Err(ReceiverHookError::Messaging(e)),
        };

        match receipt.exit_code {
            ExitCode::OK => {
                self.result_data.as_mut().unwrap().set_recipient_data(receipt.return_data);
                Ok(self.result_data.take().unwrap())
            }
            abort_code => Err(ReceiverHookError::Receiver {
                address: self.address,
                exit_code: abort_code,
                return_data: receipt.return_data,
            }),
        }
    }
}

/// Drop implements the panic if not called behaviour
impl<T: RecipientData> std::ops::Drop for ReceiverHook<T> {
    fn drop(&mut self) {
        if !self.called {
            panic!(
                "dropped before receiver hook was called on {:?} with {:?}",
                self.address, self.token_params
            );
        }
    }
}

#[cfg(test)]
mod test {
    use frc42_dispatch::method_hash;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::address::Address;

    use super::{ReceiverHook, RecipientData};
    use crate::{syscalls::FakeSyscalls, util::ActorRuntime};

    const ALICE: Address = Address::new_id(2);

    struct TestReturn;

    impl RecipientData for TestReturn {
        fn set_recipient_data(&mut self, _data: RawBytes) {}
    }

    fn generate_hook() -> ReceiverHook<TestReturn> {
        ReceiverHook::new(
            ALICE,
            RawBytes::default(),
            method_hash!("TestToken") as u32,
            TestReturn {},
        )
    }

    #[test]
    fn calls_hook() {
        let mut hook = generate_hook();
        let util = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_helper();
        assert!(util.syscalls.last_message.borrow().is_none());
        hook.call(&util).unwrap();
        assert!(util.syscalls.last_message.borrow().is_some());
    }

    #[test]
    #[should_panic]
    fn panics_if_not_called() {
        let mut _hook = generate_hook();
        // _hook should panic when dropped as we haven't called the hook
    }
}
