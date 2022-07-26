use helper::method_hash;

fn main() {
    let str_hash = method_hash!("Method");
    println!("String hash: {:x}", str_hash);

    // this one breaks naming rules and will fail to compile
    //println!("error hash: {}", method_hash!("some_function"));
}
