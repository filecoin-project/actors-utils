pub mod errors;
pub mod receiver;
pub mod state;
pub mod types;

use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::BigInt;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;

use self::errors::ActorError;
use self::state::TokenState;
use self::types::*;
use crate::runtime::Runtime;

/// A macro to abort concisely.
macro_rules! abort {
    ($code:ident, $msg:literal $(, $ex:expr)*) => {
        fvm_sdk::vm::abort(
            fvm_shared::error::ExitCode::$code.value(),
            Some(format!($msg, $($ex,)*).as_str()),
        )
    };
}

type Result<T> = std::result::Result<T, ActorError>;

/// A standard fungible token interface allowing for on-chain transactions
pub trait Token {
    /// Constructs the token
    fn constructor(&self, params: ConstructorParams) -> Result<()>;

    /// Returns the name of the token
    fn name(&self) -> String;

    /// Returns the ticker symbol of the token
    fn symbol(&self) -> String;

    /// Returns the total amount of the token in existence
    fn total_supply(&self) -> TokenAmount;

    /// Mint a number of tokens and assign them to a specific Actor
    fn mint(&self, params: MintParams) -> Result<MintReturn>;

    /// Gets the balance of a particular address (if it exists).
    fn balance_of(&self, params: Address) -> Result<TokenAmount>;

    /// Atomically increase the amount that a spender can pull from an account
    ///
    /// Returns the new allowance between those two addresses
    fn increase_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn>;

    /// Atomically decrease the amount that a spender can pull from an account
    ///
    /// The allowance cannot go below 0 and will be capped if the requested decrease
    /// is more than the current allowance
    fn decrease_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn>;

    /// Revoke the allowance between two addresses by setting it to 0
    fn revoke_allowance(&self, params: RevokeAllowanceParams) -> Result<AllowanceReturn>;

    /// Get the allowance between two addresses
    ///
    /// The spender can burn or transfer the allowance amount out of the owner's address
    fn allowance(&self, params: GetAllowanceParams) -> Result<AllowanceReturn>;

    /// Burn tokens from a specified account, decreasing the total supply
    fn burn(&self, params: BurnParams) -> Result<BurnReturn>;

    /// Transfer between two addresses
    fn transfer_from(&self, params: TransferParams) -> Result<TransferReturn>;
}

/// Holds injectable services to access/interface with IPLD/FVM layer
pub struct StandardToken<BS, FVM>
where
    BS: IpldStore + Copy,
    FVM: Runtime,
{
    /// Injected blockstore
    bs: BS,
    /// Access to the runtime
    _fvm: FVM,
}

impl<BS, FVM> StandardToken<BS, FVM>
where
    BS: IpldStore + Copy,
    FVM: Runtime,
{
    fn load_state(&self) -> TokenState {
        TokenState::load(&self.bs)
    }
}

impl<BS, FVM> Token for StandardToken<BS, FVM>
where
    BS: IpldStore + Copy,
    FVM: Runtime,
{
    fn constructor(&self, params: ConstructorParams) -> Result<()> {
        let init_state = TokenState::new(&self.bs, &params.name, &params.symbol)?;
        init_state.save(&self.bs);

        let mint_params = params.mint_params;
        self.mint(mint_params)?;
        Ok(())
    }

    fn name(&self) -> String {
        let state = self.load_state();
        state.name
    }

    fn symbol(&self) -> String {
        let state = self.load_state();
        state.symbol
    }

    fn total_supply(&self) -> TokenAmount {
        let state = self.load_state();
        state.supply
    }

    fn mint(&self, params: MintParams) -> Result<MintReturn> {
        // TODO: check we are being called in the constructor by init system actor
        // - or that other (TBD) minting rules are satified

        // these should be injectable by the token author
        let mut state = self.load_state();
        let mut balances = state.get_balance_map(&self.bs);

        // FIXME: replace fvm_sdk with abstraction
        let holder = match fvm_sdk::actor::resolve_address(&params.initial_holder) {
            Some(id) => id,
            None => {
                return Ok(MintReturn {
                    newly_minted: TokenAmount::zero(),
                    successful: false,
                    total_supply: state.supply,
                })
            }
        };

        // Mint the tokens into a specified account
        let balance = balances
            .delete(&holder)?
            .map(|de| de.1 .0)
            .unwrap_or_else(TokenAmount::zero);
        let new_balance = balance
            .checked_add(&params.value)
            .ok_or_else(|| ActorError::Arithmetic(String::from("Minting into caused overflow")))?;
        balances.set(holder, BigIntDe(new_balance))?;

        // set the global supply of the contract
        let new_supply = state.supply.checked_add(&params.value).ok_or_else(|| {
            ActorError::Arithmetic(String::from("Minting caused total supply to overflow"))
        })?;
        state.supply = new_supply;

        // commit the state if supply and balance increased
        state.save(&self.bs);

        Ok(MintReturn {
            newly_minted: params.value,
            successful: true,
            total_supply: state.supply,
        })
    }

    fn balance_of(&self, holder: Address) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let state = self.load_state();
        let balances = state.get_balance_map(&self.bs);

        // Resolve the address
        let addr_id = match fvm_sdk::actor::resolve_address(&holder) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(holder)),
        };

        match balances.get(&addr_id) {
            Ok(Some(bal)) => Ok(bal.clone().0),
            Ok(None) => Ok(TokenAmount::zero()),
            Err(err) => abort!(
                USR_ILLEGAL_STATE,
                "Failed to get balance from hamt: {:?}",
                err
            ),
        }
    }

    fn increase_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn> {
        // Load the HAMT holding balances
        let state = self.load_state();

        // FIXME: replace with runtime service call
        let caller_id = fvm_sdk::message::caller();
        let mut caller_allowances_map = state.get_actor_allowance_map(&self.bs, caller_id);

        let spender = match fvm_sdk::actor::resolve_address(&params.spender) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(params.spender)),
        };

        let new_amount = match caller_allowances_map.get(&spender)? {
            // Allowance exists - attempt to calculate new allowance
            Some(existing_allowance) => match existing_allowance.0.checked_add(&params.value) {
                Some(new_allowance) => {
                    caller_allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;
                    new_allowance
                }
                None => return Err(ActorError::Arithmetic(String::from("Allowance overflowed"))),
            },
            // No allowance recorded previously
            None => {
                caller_allowances_map.set(spender, BigIntDe(params.value.clone()))?;
                params.value
            }
        };

        state.save(&self.bs);

        Ok(AllowanceReturn {
            owner: params.owner,
            spender: params.spender,
            value: new_amount,
        })
    }

    fn decrease_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn> {
        // Load the HAMT holding balances
        let state = self.load_state();

        // FIXME: replace with runtime service call
        let caller_id = fvm_sdk::message::caller();
        let mut caller_allowances_map = state.get_actor_allowance_map(&self.bs, caller_id);
        let spender = match fvm_sdk::actor::resolve_address(&params.spender) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(params.spender)),
        };

        let new_allowance = match caller_allowances_map.get(&spender)? {
            Some(existing_allowance) => {
                let new_allowance = existing_allowance
                    .0
                    .checked_sub(&params.value)
                    .unwrap() // Unwrap should be safe as allowance always > 0
                    .max(BigInt::zero());
                caller_allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;
                new_allowance
            }
            None => {
                // Can't decrease non-existent allowance
                return Ok(AllowanceReturn {
                    owner: params.owner,
                    spender: params.spender,
                    value: TokenAmount::zero(),
                });
            }
        };

        state.save(&self.bs);

        Ok(AllowanceReturn {
            owner: params.owner,
            spender: params.spender,
            value: new_allowance,
        })
    }

    fn revoke_allowance(&self, params: RevokeAllowanceParams) -> Result<AllowanceReturn> {
        // Load the HAMT holding balances
        let state = self.load_state();

        // FIXME: replace with runtime service call
        let caller_id = fvm_sdk::message::caller();
        let mut caller_allowances_map = state.get_actor_allowance_map(&self.bs, caller_id);
        let spender = match fvm_sdk::actor::resolve_address(&params.spender) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(params.spender)),
        };

        let new_allowance = TokenAmount::zero();
        caller_allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;
        state.save(&self.bs);

        Ok(AllowanceReturn {
            owner: params.owner,
            spender: params.spender,
            value: new_allowance,
        })
    }

    fn allowance(&self, params: GetAllowanceParams) -> Result<AllowanceReturn> {
        // Load the HAMT holding balances
        let state = self.load_state();

        // FIXME: replace with runtime service call
        let owner = match fvm_sdk::actor::resolve_address(&params.owner) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(params.spender)),
        };

        let owner_allowances_map = state.get_actor_allowance_map(&self.bs, owner);
        let spender = match fvm_sdk::actor::resolve_address(&params.spender) {
            Some(id) => id,
            None => return Err(ActorError::AddrNotFound(params.spender)),
        };

        let allowance = match owner_allowances_map.get(&spender)? {
            Some(allowance) => allowance.0.clone(),
            None => TokenAmount::zero(),
        };

        Ok(AllowanceReturn {
            owner: params.owner,
            spender: params.spender,
            value: allowance,
        })
    }

    fn burn(&self, _params: BurnParams) -> Result<BurnReturn> {
        todo!()
    }

    fn transfer_from(&self, _params: TransferParams) -> Result<TransferReturn> {
        todo!()
    }
}
