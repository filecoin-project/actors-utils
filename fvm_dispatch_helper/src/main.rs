mod blake2b;

use blake2b::Blake2bHasher;
use clap::Parser;
use fvm_dispatch::hash::MethodResolver;

/// Takes a method name and converts it to an FRC-XXX compliant method number
///
/// Can be used by actor authors to precompute the method number for a given exported method to
/// avoid runtime hasing during dispatch.
#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    /// Method name to hash
    method_name: String,
}

fn main() {
    let args = Args::parse();
    let resolver = MethodResolver::new(Blake2bHasher {});
    let method_name = args.method_name;

    match resolver.method_number(method_name.as_str()) {
        Ok(method_number) => {
            println!("Method name   : {}", method_name);
            println!("Method number : {}", method_number);
        }
        Err(e) => {
            println!("Error computing method name: {:?}", e)
        }
    }
}
