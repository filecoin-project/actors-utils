mod blake2b;

use std::io::{self, BufRead};
use std::process::exit;

use blake2b::Blake2bHasher;
use clap::Parser;
use frc42_dispatch::hash::MethodResolver;

const LONG_ABOUT: &str =
    "Pass a single method name as a command line argument or a list of method names, separated by \
new-lines to stdin. The output is a list of hashes, one per method name.";

/// Takes a method name and converts it to an FRC-0042 compliant method number.
///
/// Can be used by actor authors to precompute the method number for a given exported method to
/// avoid runtime hasing during dispatch.
#[derive(Parser, Debug)]
#[clap(
    version,
    about,
    long_about = Some(LONG_ABOUT)
)]
struct Args {
    /// Method name to hash.
    method_name: Option<String>,
}

fn main() {
    let args = Args::parse();
    let resolver = MethodResolver::new(Blake2bHasher {});
    let method_name = args.method_name;

    if method_name.is_none() {
        // read from std-in if no name passed in
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut line = String::new();

        loop {
            let num_read = handle.read_line(&mut line).unwrap();
            if num_read == 0 {
                break;
            }
            let method_name = line.trim().to_string();
            let method_number = resolver.method_number(&method_name).unwrap();
            println!("{method_number}");
            line.clear();
        }

        exit(0);
    }

    match resolver.method_number(method_name.unwrap().as_str()) {
        Ok(method_number) => {
            println!("{method_number}");
        }
        Err(e) => {
            println!("Error computing method name: {e:?}")
        }
    }
}
