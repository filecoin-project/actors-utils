use helper::method_hash;

fn main() {
    // these should produce an identical result
    // because identifiers are converted to PascalCase for hashing
    let str_hash = method_hash!("Method");
    let ident_hash = method_hash!(method);
    println!("String hash: {:x}\nIdent hash:  {:x}", str_hash, ident_hash);

    // this one breaks naming rules and will fail to compile
    //println!("error hash: {}", method_hash!("some_function"));
    //println!("error hash: {}", method_hash!(some_function));
}
