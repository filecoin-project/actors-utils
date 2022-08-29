use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use num_traits::Zero;
use types::TokensReceivedParams;

use crate::runtime::messaging::{Messaging, RECEIVER_HOOK_METHOD_NUM};
use crate::token::TokenError;

pub mod types;

#[derive(Debug)]
pub struct ReceiverHookGuard {
    address: Address,
    params: TokensReceivedParams,
    called: bool,
}

impl ReceiverHookGuard {
    pub fn new(address: Address, params: TokensReceivedParams) -> Self {
        ReceiverHookGuard { address, params, called: false }
    }
    pub fn call(&mut self, msg: &dyn Messaging) -> std::result::Result<(), TokenError> {
        if self.called {
            return Err(TokenError::ReceiverHookGuardAlreadyCalled);
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

impl std::ops::Drop for ReceiverHookGuard {
    fn drop(&mut self) {
        if !self.called {
            panic!("dropped before receiver hook was called");
        }
    }
}
