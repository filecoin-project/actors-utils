use base64;
use frcxx_nft::nft::state::BatchMintReturn;
use fvm_ipld_encoding::RawBytes;

fn main() {
    let raw_bytes = base64::decode("gYoKCwwNDg8QERIT").unwrap();
    let raw_bytes = RawBytes::from(raw_bytes);
    let res = raw_bytes.deserialize::<BatchMintReturn>().unwrap();
    println!("{:?}", res);
}
