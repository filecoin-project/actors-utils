use helper::method_hash;

fn main() {
	// these should produce identical output
	let str_hash = method_hash!("Method");
    let ident_hash = method_hash!(method);

    assert_eq!(str_hash, ident_hash);
}
