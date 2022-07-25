use blake2b_simd::blake2b;
use thiserror::Error;

/// Minimal interface for a hashing function
///
/// Hasher::hash() must return a digest that is at least 4 bytes long so that it can be cast to a
/// u32
pub trait Hasher {
    /// For an input of bytes return a digest that is at least 4 bytes long
    fn hash(&self, bytes: &[u8]) -> Vec<u8>;
}

pub struct Blake2bHasher {}
impl Hasher for Blake2bHasher {
    fn hash(&self, bytes: &[u8]) -> Vec<u8> {
        blake2b(bytes).as_bytes().to_vec()
    }
}

/// Uses an underlying hashing function (blake2b by convention) to generate method numbers from
/// method names
#[derive(Default)]
pub struct MethodResolver<T: Hasher> {
    hasher: T,
}

#[derive(Error, PartialEq, Debug)]
pub enum MethodNameErr {
    #[error("empty method name provided")]
    EmptyString,
    #[error("method name does not conform to the FRCXXX convention {0}")]
    IllegalName(#[from] IllegalNameErr),
    #[error("unable to calculate method id, choose a another method name")]
    IndeterminableId,
}

#[derive(Error, PartialEq, Debug)]
pub enum IllegalNameErr {
    #[error("method name doesn't start with capital letter")]
    NotCapitalStart,
    #[error("method name contains letters outside [a-zA-Z0-9_]")]
    IllegalCharacters,
}

impl<T: Hasher> MethodResolver<T> {
    const CONSTRUCTOR_METHOD_NAME: &'static str = "Constructor";
    const CONSTRUCTOR_METHOD_NUMBER: u64 = 1_u64;
    const RESERVED_METHOD_NUMBER: u64 = 0_u64;
    const DIGEST_CHUNK_LENGTH: usize = 4;

    /// Creates a MethodResolver with an instance of a hasher (blake2b by convention)
    pub fn new(hasher: T) -> Self {
        Self { hasher }
    }

    /// Generates a standard FRC-XXX compliant method number
    ///
    /// The method number is calculated as the first four bytes of `hash(method-name)`.
    /// The name `Constructor` is always hashed to 1 and other method names that hash to
    /// 0 or 1 are avoided via rejection sampling.
    pub fn method_number(&self, method_name: &str) -> Result<u64, MethodNameErr> {
        check_method_name(method_name)?;

        if method_name == Self::CONSTRUCTOR_METHOD_NAME {
            return Ok(Self::CONSTRUCTOR_METHOD_NUMBER);
        }

        let digest = self.hasher.hash(method_name.as_bytes());

        for chunk in digest.chunks(Self::DIGEST_CHUNK_LENGTH) {
            if chunk.len() < Self::DIGEST_CHUNK_LENGTH {
                // last chunk may be smaller than 4 bytes
                break;
            }

            let method_id = as_u32(chunk) as u64;
            if method_id != Self::CONSTRUCTOR_METHOD_NUMBER
                && method_id != Self::RESERVED_METHOD_NUMBER
            {
                return Ok(method_id);
            }
        }

        Err(MethodNameErr::IndeterminableId)
    }
}

/// Checks that a method name is valid and compliant with the FRC-XXX standard recommendations
///
/// - Only ASCII characters in `[a-zA-Z0-9_]` are allowed
/// - Starts with a character in `[A-Z_]`
fn check_method_name(method_name: &str) -> Result<(), MethodNameErr> {
    if method_name.is_empty() {
        return Err(MethodNameErr::EmptyString);
    }

    // Check starts with capital letter
    let first_letter = method_name.chars().next().unwrap(); // safe because we checked for empty string
    if !first_letter.is_ascii_uppercase() {
        return Err(IllegalNameErr::NotCapitalStart.into());
    }

    // Check that all characters are legal
    if !method_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(IllegalNameErr::IllegalCharacters.into());
    }

    Ok(())
}

/// Takes a byte array and interprets it as a u32 number
///
/// Using big-endian order interperets the first four bytes to an int.
/// The slice passed to this must be at least length 4
fn as_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes[0..4].try_into().expect("bytes was not at least length 4"))
}
