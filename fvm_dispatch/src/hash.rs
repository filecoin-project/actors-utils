use fvm_sdk::crypto;

/// Minimal interface for a hashing function
///
/// Hasher::hash() must return a digest that is at least 4 bytes long so that it can be cast to a u32
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

#[derive(Default)]
pub struct MethodHasher<T: Hasher> {
    hasher: T,
}

impl<T: Hasher> MethodHasher<T> {
    const CONSTRUCTOR_METHOD_NAME: &'static str = "Constructor";
    const CONSTRUCTOR_METHOD_NUMBER: u64 = 1_u64;
    pub fn new(hasher: T) -> Self {
        Self { hasher }
    }

    pub fn method_number(&self, method_name: &str) -> u64 {
        if method_name == Self::CONSTRUCTOR_METHOD_NAME {
            Self::CONSTRUCTOR_METHOD_NUMBER
        } else {
            let digest = self.hasher.hash(method_name.as_bytes());
            if digest.len() < 4 {
                panic!("Invalid hasher used: digest must be at least 4 bytes long");
            }
            as_u32(digest.as_slice()) as u64
        }
    }
}

/// Takes a byte array and interprets it as a u32 number
/// Assumes little-endian order
#[rustfmt::skip]
fn as_u32(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << (8 * 3)) +
    ((bytes[1] as u32) << (8 * 2)) +
    ((bytes[2] as u32) << (8 * 1)) +
    (bytes[3] as u32)
}

#[cfg(test)]
mod tests {

    use super::{Blake2bSyscall, Hasher, MethodHasher};

    #[derive(Clone, Copy)]
    struct FakeHasher {}
    impl Hasher for FakeHasher {
        fn hash(&self, bytes: &[u8]) -> Vec<u8> {
            return bytes.to_vec();
        }
    }

    #[test]
    #[allow(unused)]
    fn compile() {
        let method_hasher = MethodHasher::new(Blake2bSyscall {});
    }

    #[test]
    fn constructor_method_number() {
        let method_hasher = MethodHasher::new(FakeHasher {});
        assert_eq!(method_hasher.method_number("Constructor"), 1);
    }

    #[test]
    fn normal_method_number() {
        let fake_hasher = FakeHasher {};
        let method_hasher = MethodHasher::new(fake_hasher);
        assert_eq!(
            method_hasher.method_number("NormalMethod"),
            super::as_u32(&fake_hasher.hash(b"NormalMethod")) as u64
        );
    }
}
