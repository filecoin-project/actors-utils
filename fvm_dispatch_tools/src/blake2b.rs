use fvm_dispatch::hash::Hasher;
use multihash::Code;
use multihash::MultihashDigest;
pub struct Blake2bHasher {}
impl Hasher for Blake2bHasher {
    fn hash(&self, bytes: &[u8]) -> Vec<u8> {
        let digest = Code::Blake2b256.digest(bytes);
        // drop the first 4 bytes of the multihash which identify the hash as blake2b256
        digest.to_bytes()[4..].to_vec()
    }
}
