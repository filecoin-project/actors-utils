use thiserror::Error;

use fvm_sdk::crypto;

/// Minimal interface for a hashing function
///
/// Hasher::hash() must return a digest that is at least 4 bytes long so that it can be cast to a
/// u32
pub trait Hasher {
    /// For an input of bytes return a digest that is at least 4 bytes long
    fn hash(&self, bytes: &[u8]) -> Vec<u8>;
}

/// Hasher that uses the hash_blake2b syscall provided by the FVM
#[derive(Default)]
pub struct Blake2bSyscall {}

impl Hasher for Blake2bSyscall {
    fn hash(&self, bytes: &[u8]) -> Vec<u8> {
        crypto::hash_blake2b(bytes).try_into().unwrap()
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
    #[error("illegal symbol used in method name")]
    IllegalSymbol,
    #[error("unable to calculate method id, choose a another method name")]
    IndeterminableId,
}

impl<T: Hasher> MethodResolver<T> {
    const CONSTRUCTOR_METHOD_NAME: &'static str = "Constructor";
    const CONSTRUCTOR_METHOD_NUMBER: u64 = 1_u64;
    const RESERVED_METHOD_NUMBER: u64 = 0_u64;

    /// Creates a MethodResolver with an instance of a hasher (blake2b by convention)
    pub fn new(hasher: T) -> Self {
        Self { hasher }
    }

    /// Generates a standard FRC-XXX compliant method number
    ///
    /// The method number is calculated as the first four bytes of `hash(method-name)`.
    /// The name `Constructor` is always hashed to 1 and other method names that hash to
    /// 0 or 1 are avoided via rejection sampling.
    ///
    ///
    pub fn method_number(&self, method_name: &str) -> Result<u64, MethodNameErr> {
        // TODO: sanitise method_name before checking (or reject invalid whitespace)
        if method_name.contains('|') {
            return Err(MethodNameErr::IllegalSymbol);
        }

        if method_name.is_empty() {
            return Err(MethodNameErr::EmptyString);
        }

        if method_name == Self::CONSTRUCTOR_METHOD_NAME {
            return Ok(Self::CONSTRUCTOR_METHOD_NUMBER);
        }
        let mut digest = self.hasher.hash(method_name.as_bytes());
        while digest.len() >= 4 {
            let method_id = as_u32(digest.as_slice()) as u64;
            if method_id != Self::CONSTRUCTOR_METHOD_NUMBER
                && method_id != Self::RESERVED_METHOD_NUMBER
            {
                return Ok(method_id);
            } else {
                digest.remove(0);
            }
        }
        Err(MethodNameErr::IndeterminableId)
    }
}

/// Takes a byte array and interprets it as a u32 number
///
/// Using big-endian order interperets the first four bytes to an int.
/// The slice passed to this must be at least length 4
fn as_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(
        bytes[0..4]
            .try_into()
            .expect("bytes was not at least length 4"),
    )
}

#[cfg(test)]
mod tests {

    use super::{Hasher, MethodNameErr, MethodResolver};

    #[derive(Clone, Copy)]
    struct FakeHasher {}
    impl Hasher for FakeHasher {
        fn hash(&self, bytes: &[u8]) -> Vec<u8> {
            bytes.to_vec()
        }
    }

    #[test]
    fn constructor_is_1() {
        let method_hasher = MethodResolver::new(FakeHasher {});
        assert_eq!(method_hasher.method_number("Constructor").unwrap(), 1);
    }

    #[test]
    fn normal_method_is_hashed() {
        let fake_hasher = FakeHasher {};
        let method_hasher = MethodResolver::new(fake_hasher);
        assert_eq!(
            method_hasher.method_number("NormalMethod").unwrap(),
            super::as_u32(&fake_hasher.hash(b"NormalMethod")) as u64
        );

        assert_eq!(
            method_hasher.method_number("NormalMethod2").unwrap(),
            super::as_u32(&fake_hasher.hash(b"NormalMethod2")) as u64
        );
    }

    #[test]
    fn disallows_invalid_method_names() {
        let method_hasher = MethodResolver::new(FakeHasher {});
        assert_eq!(
            method_hasher.method_number("Invalid|Method").unwrap_err(),
            MethodNameErr::IllegalSymbol
        );
        assert_eq!(
            method_hasher.method_number("").unwrap_err(),
            MethodNameErr::EmptyString
        );
    }

    #[test]
    fn avoids_disallowed_method_numbers() {
        let hasher = FakeHasher {};
        let method_hasher = MethodResolver::new(hasher);

        // This simulates a method name that would hash to 0
        let contrived_0 = "\0\0\0\0MethodName";
        let contrived_0_digest = hasher.hash(contrived_0.as_bytes());
        assert_eq!(super::as_u32(&contrived_0_digest), 0);
        // But the method number is not a collision
        assert_ne!(method_hasher.method_number(contrived_0).unwrap(), 0);

        // This simulates a method name that would hash to 1
        let contrived_1 = "\0\0\0\x01MethodName";
        let contrived_1_digest = hasher.hash(contrived_1.as_bytes());
        assert_eq!(super::as_u32(&contrived_1_digest), 1);
        // But the method number is not a collision
        assert_ne!(method_hasher.method_number(contrived_1).unwrap(), 1);
    }
}
