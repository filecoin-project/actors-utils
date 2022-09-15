use serde_tuple::{Deserialize_tuple, Serialize_tuple};

use super::state::TokenID;

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct BatchMintReturn {
    pub tokens: Vec<TokenID>,
}
