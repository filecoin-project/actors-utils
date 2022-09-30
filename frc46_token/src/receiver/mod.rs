use cid::Cid;
use fvm_actor_utils::messaging::{Messaging, RECEIVER_HOOK_METHOD_NUM};
use fvm_ipld_encoding::RawBytes;
#[cfg(target_family = "wasm")]
use fvm_sdk as sdk;
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use num_traits::Zero;
use types::{FRC46TokenReceived, UniversalReceiverParams, FRC46_TOKEN_TYPE};

use crate::token::TokenError;

pub mod types;

pub trait ReceiverData {
    fn set_recipient_data(&mut self, data: RawBytes);
    fn set_new_root(&mut self, new_root: Option<Cid>);
    fn new_root(&self) -> Option<Cid>;
}

/// Implements a guarded call to a token receiver hook
///
/// Mint and Transfer operations will return this so that state can be updated and saved
/// before making the call into the receiver hook.
///
/// This also tracks whether the call has been made or not, and
/// will panic if dropped without calling the hook.
#[derive(Debug)]
pub struct ReceiverHook<T: ReceiverData> {
    address: Address,
    params: FRC46TokenReceived,
    called: bool,
    result_data: Option<T>,
}

impl<T: ReceiverData> ReceiverHook<T> {
    /// Construct a new ReceiverHook call
    pub fn new(address: Address, params: FRC46TokenReceived, result_data: T) -> Self {
        ReceiverHook { address, params, called: false, result_data: Some(result_data) }
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
    pub fn call(&mut self, msg: &dyn Messaging) -> std::result::Result<T, TokenError> {
        // TODO: this stuff should be implemented elsewhere, or we don't do it here at all
        #[cfg(target_family = "wasm")]
        fn get_root() -> Cid {
            sdk::sself::root().unwrap()
        }
        // stub version allows us to build and run unit tests
        #[cfg(not(target_family = "wasm"))]
        fn get_root() -> Cid {
            Cid::default()
        }

        if self.called {
            return Err(TokenError::ReceiverHookAlreadyCalled);
        }

        self.called = true;

        let params = UniversalReceiverParams {
            type_: FRC46_TOKEN_TYPE,
            payload: RawBytes::serialize(&self.params)?,
        };

        let before_cid = get_root();

        let receipt = msg.send(
            &self.address,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(&params)?,
            &TokenAmount::zero(),
        )?;

        let after_cid = get_root();

        match receipt.exit_code {
            ExitCode::OK => {
                let mut result = self.result_data.take().unwrap();
                //self.result_data.as_mut().unwrap().set_recipient_data(receipt.return_data);
                result.set_recipient_data(receipt.return_data);
                let new_root = if before_cid == after_cid { None } else { Some(after_cid) };
                result.set_new_root(new_root);
                Ok(result)
            }
            abort_code => Err(TokenError::ReceiverHook {
                from: self.params.from,
                to: self.params.to,
                operator: self.params.operator,
                amount: self.params.amount.clone(),
                exit_code: abort_code,
            }),
        }
    }
}

/// Drop implements the panic if not called behaviour
impl<T: ReceiverData> std::ops::Drop for ReceiverHook<T> {
    fn drop(&mut self) {
        if !self.called {
            panic!(
                "dropped before receiver hook was called on {:?} with {:?}",
                self.address, self.params
            );
        }
    }
}

#[cfg(test)]
mod test {
    use cid::Cid;
    use fvm_actor_utils::messaging::FakeMessenger;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::{address::Address, econ::TokenAmount};
    use num_traits::Zero;

    use super::{types::FRC46TokenReceived, ReceiverData, ReceiverHook};

    const TOKEN_ACTOR: Address = Address::new_id(1);
    const ALICE: Address = Address::new_id(2);

    struct TestReturn;
    impl ReceiverData for TestReturn {
        fn set_recipient_data(&mut self, _data: RawBytes) {}
        fn set_new_root(&mut self, _new_root: Option<Cid>) {}
        fn new_root(&self) -> Option<Cid> {
            None
        }
    }

    fn generate_hook() -> ReceiverHook<TestReturn> {
        let params = FRC46TokenReceived {
            operator: TOKEN_ACTOR.id().unwrap(),
            from: TOKEN_ACTOR.id().unwrap(),
            to: ALICE.id().unwrap(),
            amount: TokenAmount::zero(),
            operator_data: RawBytes::default(),
            token_data: RawBytes::default(),
        };
        ReceiverHook::new(ALICE, params, TestReturn {})
    }

    #[test]
    fn calls_hook() {
        let mut hook = generate_hook();
        let msg = FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 3);
        hook.call(&msg).unwrap();
    }

    #[test]
    #[should_panic]
    fn panics_if_not_called() {
        let mut _hook = generate_hook();
        // _hook should panic when dropped as we haven't called the hook
    }
}
