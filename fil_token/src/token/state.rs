use anyhow::anyhow;
use cid::multihash::Code;
use cid::Cid;

use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Hamt;
use fvm_sdk::sself;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use fvm_shared::HAMT_BIT_WIDTH;

/// A macro to abort concisely.
macro_rules! abort {
    ($code:ident, $msg:literal $(, $ex:expr)*) => {
        fvm_sdk::vm::abort(
            fvm_shared::error::ExitCode::$code.value(),
            Some(format!($msg, $($ex,)*).as_str()),
        )
    };
}

/// Token state ipld structure
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TokenState {
    /// Total supply of token
    #[serde(with = "bigint_ser")]
    pub supply: TokenAmount,
    /// Token name
    pub name: String,
    /// Ticker symbol
    pub symbol: String,

    /// Map<ActorId, TokenAmount> of balances as a Hamt
    pub balances: Cid,
    /// Map<ActorId, Map<ActorId, TokenAmount>> as a Hamt
    pub allowances: Cid,
}

/// Functions to get and modify token state to and from the IPLD layer
impl TokenState {
    pub fn new<BS: IpldStore>(store: &BS, name: &str, symbol: &str) -> StateResult<Self> {
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

    /// Loads a fresh copy of the state from a blockstore
    pub fn load<BS: IpldStore>(bs: &BS) -> Self {
        // First, load the current state root
        // TODO: inject sself capabilities
        let root = match sself::root() {
            Ok(root) => root,
            Err(err) => abort!(USR_ILLEGAL_STATE, "failed to get root: {:?}", err),
        };

        // Load the actor state from the state tree.
        match bs.get_cbor::<Self>(&root) {
            Ok(Some(state)) => state,
            Ok(None) => abort!(USR_ILLEGAL_STATE, "state does not exist"),
            Err(err) => abort!(USR_ILLEGAL_STATE, "failed to get state: {}", err),
        }
    }

    /// Saves the current state to the blockstore
    pub fn save<BS: IpldStore + Copy>(&self, bs: &BS) -> Cid {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => abort!(USR_SERIALIZATION, "failed to serialize state: {:?}", err),
        };
        let block = Block {
            codec: DAG_CBOR,
            data: serialized,
        };
        let cid = match bs.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => abort!(USR_SERIALIZATION, "failed to store initial state: {:}", err),
        };
        if let Err(err) = sself::set_root(&cid) {
            abort!(USR_ILLEGAL_STATE, "failed to set root ciid: {:}", err);
        }
        cid
    }

    pub fn get_balance_map<BS: IpldStore + Copy>(&self, bs: &BS) -> Hamt<BS, BigIntDe, ActorID> {
        match Hamt::<BS, BigIntDe, ActorID>::load(&self.balances, *bs) {
            Ok(map) => map,
            Err(err) => abort!(USR_ILLEGAL_STATE, "Failed to load balances hamt: {:?}", err),
        }
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    pub fn get_allowances_map<BS: IpldStore + Copy>(&self, bs: &BS) -> Hamt<BS, Cid, ActorID> {
        match Hamt::<BS, Cid, ActorID>::load(&self.allowances, *bs) {
            Ok(map) => map,
            Err(err) => abort!(
                USR_ILLEGAL_STATE,
                "Failed to load allowances hamt: {:?}",
                err
            ),
        }
    }

    /// Get the allowances map of a specific actor, lazily creating one if it didn't exist
    pub fn get_actor_allowance_map<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
        owner: ActorID,
    ) -> Hamt<BS, BigIntDe, ActorID> {
        let mut global_allowances = self.get_allowances_map(bs);
        match global_allowances.get(&owner) {
            Ok(Some(map)) => {
                // authorising actor already had an allowance map, return it
                Hamt::<BS, BigIntDe, ActorID>::load(map, *bs).unwrap()
            }
            Ok(None) => {
                // authorising actor does not have an allowance map, create one and return it
                let mut new_actor_allowances = Hamt::new(*bs);
                let cid = new_actor_allowances
                    .flush()
                    .map_err(|e| anyhow!("Failed to create empty balances map state {}", e))
                    .unwrap();
                global_allowances.set(owner, cid).unwrap();
                new_actor_allowances
            }
            Err(e) => abort!(
                USR_ILLEGAL_STATE,
                "failed to get actor's allowance map {:?}",
                e
            ),
        }
    }

    // fn enough_allowance(
    //     &self,
    //     bs: &Blockstore,
    //     from: ActorID,
    //     spender: ActorID,
    //     to: ActorID,
    //     amount: &TokenAmount,
    // ) -> std::result::Result<(), TokenAmountDiff> {
    //     if spender == from {
    //         return std::result::Result::Ok(());
    //     }

    //     let allowances = self.get_actor_allowance_map(bs, from);
    //     let allowance = match allowances.get(&to) {
    //         Ok(Some(amount)) => amount.0.clone(),
    //         _ => TokenAmount::zero(),
    //     };

    //     if allowance.lt(&amount) {
    //         Err(TokenAmountDiff {
    //             actual: allowance,
    //             required: amount.clone(),
    //         })
    //     } else {
    //         std::result::Result::Ok(())
    //     }
    // }

    // fn enough_balance(
    //     &self,
    //     bs: &Blockstore,
    //     from: ActorID,
    //     amount: &TokenAmount,
    // ) -> std::result::Result<(), TokenAmountDiff> {
    //     let balances = self.get_balance_map(bs);
    //     let balance = match balances.get(&from) {
    //         Ok(Some(amount)) => amount.0.clone(),
    //         _ => TokenAmount::zero(),
    //     };

    //     if balance.lt(&amount) {
    //         Err(TokenAmountDiff {
    //             actual: balance,
    //             required: amount.clone(),
    //         })
    //     } else {
    //         std::result::Result::Ok(())
    //     }
    // }

    // /// Atomically make a transfer
    // fn make_transfer(
    //     &self,
    //     bs: &Blockstore,
    //     amount: &TokenAmount,
    //     from: ActorID,
    //     spender: ActorID,
    //     to: ActorID,
    // ) -> TransferResult<TokenAmount> {
    //     if let Err(e) = self.enough_allowance(bs, from, spender, to, amount) {
    //         return Err(TransferError::InsufficientAllowance(e));
    //     }
    //     if let Err(e) = self.enough_balance(bs, from, amount) {
    //         return Err(TransferError::InsufficientBalance(e));
    //     }

    //     // Decrease allowance, decrease balance
    //     // From the above checks, we know these exist
    //     // TODO: do this in a transaction to avoid re-entrancy bugs
    //     let mut allowances = self.get_actor_allowance_map(bs, from);
    //     let allowance = allowances.get(&to).unwrap().unwrap();
    //     let new_allowance = allowance.0.clone().sub(amount);
    //     allowances.set(to, BigIntDe(new_allowance)).unwrap();

    //     let mut balances = self.get_balance_map(bs);
    //     let sender_balance = balances.get(&from).unwrap().unwrap();
    //     let new_sender_balance = sender_balance.0.clone().sub(amount);
    //     balances.set(from, BigIntDe(new_sender_balance)).unwrap();

    //     // TODO: call the receive hook

    //     // TODO: if no hook, revert the balance and allowance change

    //     // if successful, mark the balance as having been credited

    //     let receiver_balance = balances.get(&to).unwrap().unwrap();
    //     let new_receiver_balance = receiver_balance.0.clone().add(amount);
    //     balances.set(to, BigIntDe(new_receiver_balance)).unwrap();

    //     Ok(amount.clone())
    // }
}

impl Cbor for TokenState {}

pub struct TokenAmountDiff {
    pub required: TokenAmount,
    pub actual: TokenAmount,
}

pub enum TransferError {
    NoRecrHook,
    InsufficientAllowance(TokenAmountDiff),
    InsufficientBalance(TokenAmountDiff),
}

pub enum StateError {
    AddrNotFound(Address),
    Arithmetic,
    Ipld(fvm_ipld_hamt::Error),
    Err(anyhow::Error),
    Transfer(TransferError),
}

pub type StateResult<T> = std::result::Result<T, StateError>;

impl From<anyhow::Error> for StateError {
    fn from(e: anyhow::Error) -> Self {
        Self::Err(e)
    }
}

impl From<fvm_ipld_hamt::Error> for StateError {
    fn from(e: fvm_ipld_hamt::Error) -> Self {
        Self::Ipld(e)
    }
}

impl From<TransferError> for StateError {
    fn from(e: TransferError) -> Self {
        Self::Transfer(e)
    }
}
