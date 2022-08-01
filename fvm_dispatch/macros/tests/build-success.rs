use fvm_dispatch_macros::method_hash;

fn main() {
    let str_hash = method_hash!("Method");

    assert_eq!(str_hash, 1253606847);
}
