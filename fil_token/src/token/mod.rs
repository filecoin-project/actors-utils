pub mod receiver;

use std::ops::Add;
use std::ops::Sub;

use anyhow::anyhow;
use cid::Cid;

use cid::multihash::Code;

use fvm_ipld_blockstore::Blockstore as Store;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Hamt;

use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::BigInt;
use fvm_shared::bigint::Zero;
pub use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use fvm_shared::HAMT_BIT_WIDTH;

use fvm_sdk::ipld;
use fvm_sdk::sself;

use crate::blockstore::Blockstore;

pub type Result<T> = std::result::Result<T, TokenError>;

type TransferResult<T> = std::result::Result<T, TransferError>;

pub struct TokenAmountDiff {
    pub required: TokenAmount,
    pub actual: TokenAmount,
}

pub enum TransferError {
    NoRecrHook,
    InsufficientAllowance(TokenAmountDiff),
    InsufficientBalance(TokenAmountDiff),
}

pub enum TokenError {
    AddrNotFound(Address),
    Arithmetic,
    Ipld(fvm_ipld_hamt::Error),
    Err(anyhow::Error),
    Transfer(TransferError),
}

impl From<anyhow::Error> for TokenError {
    fn from(e: anyhow::Error) -> Self {
        Self::Err(e)
    }
}

impl From<fvm_ipld_hamt::Error> for TokenError {
    fn from(e: fvm_ipld_hamt::Error) -> Self {
        Self::Ipld(e)
    }
}

impl From<TransferError> for TokenError {
    fn from(e: TransferError) -> Self {
        Self::Transfer(e)
    }
}

/// A macro to abort concisely.
macro_rules! abort {
    ($code:ident, $msg:literal $(, $ex:expr)*) => {
        fvm_sdk::vm::abort(
            fvm_shared::error::ExitCode::$code.value(),
            Some(format!($msg, $($ex,)*).as_str()),
        )
    };
}

/// A standard fungible token interface allowing for on-chain transactions
pub trait Token {
    fn name(&self) -> String;

    fn symbol(&self) -> String;

    fn total_supply(&self) -> TokenAmount;

    /// Mint a number of tokens and assign them to a specific Actor
    ///
    /// Minting can only be done in the constructor as once off
    /// TODO: allow authorised actors to mint more supply
    fn mint(
        &self,
        amount: TokenAmount,
        initial_holder: Address,
        bs: &Blockstore,
    ) -> Result<TokenAmount>;

    /// Gets the balance of a particular address (if it exists).
    fn balance_of(&self, holder: Address, bs: &Blockstore) -> Result<TokenAmount>;

    /// Atomically increase the amount that a spender can pull from an account
    fn increase_allowance(
        &self,
        spender: Address,
        value: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount>;

    /// Atomically decrease the amount that a spender can pull from an account
    ///
    /// The allowance cannot go below 0 and will be capped if the requested decrease
    /// is more than the current allowance
    fn decrease_allowance(
        &self,
        spender: Address,
        value: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount>;

    fn revoke_allowance(&self, spender: Address, bs: &Blockstore) -> Result<()>;

    fn allowance(&self, owner: Address, spender: Address, bs: &Blockstore) -> Result<TokenAmount>;

    fn burn(&self, amount: TokenAmount, data: &[u8], bs: &Blockstore) -> Result<TokenAmount>;

    fn transfer_from(
        &self,
        owner: Address,
        spender: Address,
        amount: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount>;

    fn burn_from(
        &self,
        from: Address,
        amount: TokenAmount,
        data: &[u8],
        bs: &Blockstore,
    ) -> Result<TokenAmount>;
}

/// Token state ipld structure
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct DefaultToken {
    #[serde(with = "bigint_ser")]
    supply: TokenAmount,
    name: String,
    symbol: String,

    balances: Cid,
    allowances: Cid,
}

/// Default token implementation
impl DefaultToken {
    pub fn new<BS>(name: &str, symbol: &str, store: &BS) -> anyhow::Result<Self>
    where
        BS: Store,
    {
        let empty_balance_map = Hamt::<_, ()>::new_with_bit_width(store, HAMT_BIT_WIDTH)
            .flush()
            .map_err(|e| anyhow!("Failed to create empty balances map state {}", e))?;
        let empty_allowances_map = Hamt::<_, ()>::new_with_bit_width(store, HAMT_BIT_WIDTH)
            .flush()
            .map_err(|e| anyhow!("Failed to create empty balances map state {}", e))?;

        Ok(Self {
            supply: Default::default(),
            name: name.to_string(),
            symbol: symbol.to_string(),
            balances: empty_balance_map,
            allowances: empty_allowances_map,
        })
    }

    pub fn load() -> Self {
        // First, load the current state root.
        let root = match sself::root() {
            Ok(root) => root,
            Err(err) => abort!(USR_ILLEGAL_STATE, "failed to get root: {:?}", err),
        };

        // Load the actor state from the state tree.
        match Blockstore.get_cbor::<Self>(&root) {
            Ok(Some(state)) => state,
            Ok(None) => abort!(USR_ILLEGAL_STATE, "state does not exist"),
            Err(err) => abort!(USR_ILLEGAL_STATE, "failed to get state: {}", err),
        }
    }

    pub fn save(&self) -> Cid {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => abort!(USR_SERIALIZATION, "failed to serialize state: {:?}", err),
        };
        let cid = match ipld::put(Code::Blake2b256.into(), 32, DAG_CBOR, serialized.as_slice()) {
            Ok(cid) => cid,
            Err(err) => abort!(USR_SERIALIZATION, "failed to store initial state: {:}", err),
        };
        if let Err(err) = sself::set_root(&cid) {
            abort!(USR_ILLEGAL_STATE, "failed to set root ciid: {:}", err);
        }
        cid
    }

    fn get_balance_map(&self, bs: &Blockstore) -> Hamt<Blockstore, BigIntDe, ActorID> {
        let balances = match Hamt::<Blockstore, BigIntDe, ActorID>::load(&self.balances, *bs) {
            Ok(map) => map,
            Err(err) => abort!(USR_ILLEGAL_STATE, "Failed to load balances hamt: {:?}", err),
        };
        balances
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    fn get_allowances_map(&self, bs: &Blockstore) -> Hamt<Blockstore, Cid, ActorID> {
        let allowances = match Hamt::<Blockstore, Cid, ActorID>::load(&self.allowances, *bs) {
            Ok(map) => map,
            Err(err) => abort!(
                USR_ILLEGAL_STATE,
                "Failed to load allowances hamt: {:?}",
                err
            ),
        };
        allowances
    }

    /// Get the allowances map of a specific actor, lazily creating one if it didn't exist
    fn get_actor_allowance_map(
        &self,
        bs: &Blockstore,
        authoriser: ActorID,
    ) -> Hamt<Blockstore, BigIntDe, ActorID> {
        let mut global_allowances = self.get_allowances_map(bs);
        match global_allowances.get(&authoriser) {
            Ok(Some(map)) => {
                // authorising actor already had an allowance map, return it
                Hamt::<Blockstore, BigIntDe, ActorID>::load(map, *bs).unwrap()
            }
            Ok(None) => {
                // authorising actor does not have an allowance map, create one and return it
                let mut new_actor_allowances = Hamt::new(*bs);
                let cid = new_actor_allowances
                    .flush()
                    .map_err(|e| anyhow!("Failed to create empty balances map state {}", e))
                    .unwrap();
                global_allowances.set(authoriser, cid).unwrap();
                new_actor_allowances
            }
            Err(e) => abort!(
                USR_ILLEGAL_STATE,
                "failed to get actor's allowance map {:?}",
                e
            ),
        }
    }

    fn enough_allowance(
        &self,
        from: ActorID,
        spender: ActorID,
        to: ActorID,
        amount: &TokenAmount,
        bs: &Blockstore,
    ) -> std::result::Result<(), TokenAmountDiff> {
        if spender == from {
            return std::result::Result::Ok(());
        }

        let allowances = self.get_actor_allowance_map(bs, from);
        let allowance = match allowances.get(&to) {
            Ok(Some(amount)) => amount.0.clone(),
            _ => TokenAmount::zero(),
        };

        if allowance.lt(&amount) {
            Err(TokenAmountDiff {
                actual: allowance,
                required: amount.clone(),
            })
        } else {
            std::result::Result::Ok(())
        }
    }

    fn enough_balance(
        &self,
        from: ActorID,
        amount: &TokenAmount,
        bs: &Blockstore,
    ) -> std::result::Result<(), TokenAmountDiff> {
        let balances = self.get_balance_map(bs);
        let balance = match balances.get(&from) {
            Ok(Some(amount)) => amount.0.clone(),
            _ => TokenAmount::zero(),
        };

        if balance.lt(&amount) {
            Err(TokenAmountDiff {
                actual: balance,
                required: amount.clone(),
            })
        } else {
            std::result::Result::Ok(())
        }
    }

    /// Atomically make a transfer
    fn make_transfer(
        &self,
        bs: &Blockstore,
        amount: &TokenAmount,
        from: ActorID,
        spender: ActorID,
        to: ActorID,
    ) -> TransferResult<TokenAmount> {
        if let Err(e) = self.enough_allowance(from, spender, to, amount, bs) {
            return Err(TransferError::InsufficientAllowance(e));
        }
        if let Err(e) = self.enough_balance(from, amount, bs) {
            return Err(TransferError::InsufficientBalance(e));
        }

        // Decrease allowance, decrease balance
        // From the above checks, we know these exist
        // TODO: do this in a transaction to avoid re-entrancy bugs
        let mut allowances = self.get_actor_allowance_map(bs, from);
        let allowance = allowances.get(&to).unwrap().unwrap();
        let new_allowance = allowance.0.clone().sub(amount);
        allowances.set(to, BigIntDe(new_allowance)).unwrap();

        let mut balances = self.get_balance_map(bs);
        let sender_balance = balances.get(&from).unwrap().unwrap();
        let new_sender_balance = sender_balance.0.clone().sub(amount);
        balances.set(from, BigIntDe(new_sender_balance)).unwrap();

        // TODO: call the receive hook

        // TODO: if no hook, revert the balance and allowance change

        // if successful, mark the balance as having been credited

        let receiver_balance = balances.get(&to).unwrap().unwrap();
        let new_receiver_balance = receiver_balance.0.clone().add(amount);
        balances.set(to, BigIntDe(new_receiver_balance)).unwrap();

        Ok(amount.clone())
    }
}

fn resolve_address(address: &Address) -> Result<ActorID> {
    match fvm_sdk::actor::resolve_address(address) {
        Some(addr) => Ok(addr),
        None => Err(TokenError::AddrNotFound(*address)),
    }
}

impl Cbor for DefaultToken {}

impl Token for DefaultToken {
    fn name(&self) -> String {
        let state = Self::load();
        state.name
    }

    fn symbol(&self) -> String {
        let state = Self::load();
        state.symbol
    }

    fn total_supply(&self) -> TokenAmount {
        let state = Self::load();
        state.supply
    }

    fn mint(&self, amount: TokenAmount, treasury: Address, bs: &Blockstore) -> Result<TokenAmount> {
        // TODO: check we are being called in the constructor by init system actor

        let mut state = Self::load();
        let mut balances = self.get_balance_map(bs);

        let treasury = match fvm_sdk::actor::resolve_address(&treasury) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(treasury)),
        };

        // Mint the tokens into a specified account
        balances.set(treasury, BigIntDe(amount.clone()))?;

        // set the global supply of the contract
        state.supply = amount.clone();

        Ok(amount)
    }

    fn balance_of(&self, holder: Address, bs: &Blockstore) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let balances = self.get_balance_map(bs);

        // Resolve the address
        let addr_id = match fvm_sdk::actor::resolve_address(&holder) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(holder)),
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

    fn increase_allowance(
        &self,
        spender: Address,
        value: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount> {
        let caller_id = fvm_sdk::message::caller();

        let caller_allowances_map = self.get_actor_allowance_map(bs, caller_id);

        let spender = match fvm_sdk::actor::resolve_address(&spender) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(spender)),
        };

        let new_amount = match caller_allowances_map.get(&spender)? {
            Some(existing_allowance) => existing_allowance.0.checked_add(&value),
            None => todo!(),
        };

        match new_amount {
            Some(amount) => Ok(amount),
            None => Err(TokenError::Arithmetic),
        }
    }

    fn decrease_allowance(
        &self,
        spender: Address,
        value: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount> {
        let caller_id = fvm_sdk::message::caller();

        // TODO: can exit earlier if the authorisers map doesn't even exist
        let mut caller_allowances_map = self.get_actor_allowance_map(bs, caller_id);

        let spender = match fvm_sdk::actor::resolve_address(&spender) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(spender)),
        };

        // check that the allowance is larger than the decrease
        let existing_balance = match caller_allowances_map.get(&spender)? {
            Some(existing_allowance) => {
                if existing_allowance.0.gt(&value) {
                    Some(existing_allowance.clone().0)
                } else {
                    None
                }
            }
            None => None,
        };

        match existing_balance {
            Some(existing_balance) => {
                let new_allowance = existing_balance.sub(value);
                caller_allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;
                Ok(new_allowance)
            }
            _ => {
                caller_allowances_map.set(spender, BigIntDe(BigInt::zero()))?;
                Ok(BigInt::zero())
            }
        }
    }

    fn revoke_allowance(&self, _spender: Address, _bs: &Blockstore) -> Result<()> {
        todo!()
    }

    fn allowance(&self, owner: Address, spender: Address, bs: &Blockstore) -> Result<TokenAmount> {
        let owner = match fvm_sdk::actor::resolve_address(&owner) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(owner)),
        };
        let spender = match fvm_sdk::actor::resolve_address(&spender) {
            Some(id) => id,
            None => return Err(TokenError::AddrNotFound(spender)),
        };

        let allowance_map = self.get_actor_allowance_map(bs, owner);
        match allowance_map.get(&spender)? {
            Some(allowance) => Ok(allowance.0.clone()),
            None => Ok(TokenAmount::zero()),
        }
    }

    fn burn(&self, _amount: TokenAmount, _data: &[u8], _bs: &Blockstore) -> Result<TokenAmount> {
        todo!()
    }

    fn transfer_from(
        &self,
        owner: Address,
        receiver: Address,
        amount: TokenAmount,
        bs: &Blockstore,
    ) -> Result<TokenAmount> {
        let spender = fvm_sdk::message::caller();
        let owner = resolve_address(&owner)?;
        let receiver = resolve_address(&receiver)?;

        let res = self.make_transfer(bs, &amount, owner, spender, receiver);
        match res {
            Ok(amount) => Ok(amount),
            Err(e) => Err(e.into()),
        }
    }

    fn burn_from(
        &self,
        _from: Address,
        _amount: TokenAmount,
        _data: &[u8],
        _bs: &Blockstore,
    ) -> Result<TokenAmount> {
        todo!()
    }
}
