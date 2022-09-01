use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use num_traits::Zero;
use types::TokensReceivedParams;

use crate::runtime::messaging::{Messaging, RECEIVER_HOOK_METHOD_NUM};
use crate::token::TokenError;

pub mod types;

/// Implements a guarded call to a token receiver hook
///
/// Mint and Transfer operations will return this so that state can be updated and saved
/// before making the call into the receiver hook.
///
/// This also tracks whether the call has been made or not, and
/// will panic if dropped without calling the hook.
#[derive(Debug)]
pub struct ReceiverHook {
    address: Address,
    params: TokensReceivedParams,
    called: bool,
}

impl ReceiverHook {
    /// Construct a new ReceiverHook call
    pub fn new(address: Address, params: TokensReceivedParams) -> Self {
        ReceiverHook { address, params, called: false }
    }
    /// Call the receiver hook and return the result
    ///
    /// Requires the same Messaging trait as the Token
    /// eg: `hook.call(token.msg())?;`
    ///
    /// Returns an error if already called
    pub fn call(&mut self, msg: &dyn Messaging) -> std::result::Result<(), TokenError> {
        if self.called {
            return Err(TokenError::ReceiverHookAlreadyCalled);
        }

        self.called = true;

        let receipt = msg.send(
            &self.address,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(&self.params)?,
            &TokenAmount::zero(),
        )?;

        match receipt.exit_code {
            ExitCode::OK => Ok(()),
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
impl std::ops::Drop for ReceiverHook {
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
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::{address::Address, econ::TokenAmount};
    use num_traits::Zero;

    use super::{types::TokensReceivedParams, ReceiverHook};
    use crate::runtime::messaging::FakeMessenger;

    const TOKEN_ACTOR: Address = Address::new_id(1);
    const ALICE: Address = Address::new_id(2);

    fn generate_hook() -> ReceiverHook {
        let params = TokensReceivedParams {
            operator: TOKEN_ACTOR.id().unwrap(),
            from: TOKEN_ACTOR.id().unwrap(),
            to: ALICE.id().unwrap(),
            amount: TokenAmount::zero(),
            operator_data: RawBytes::default(),
            token_data: RawBytes::default(),
        };
        ReceiverHook::new(ALICE, params)
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
