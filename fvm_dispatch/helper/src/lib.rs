use proc_macro::TokenStream;

use convert_case::{Case, Casing};
use fvm_dispatch::hash::MethodResolver;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Ident, LitStr, Result};

mod hash;
use crate::hash::Blake2bHasher;

enum MethodName {
    Ident(Ident),
    Text(LitStr),
}

impl MethodName {
    /// Hash the method name
    ///
    /// - Text (string) gets hashed as-is
    /// - Identifiers (function names) get converted to PascalCase to meet naming rules
    fn hash(&self) -> u64 {
        let resolver = MethodResolver::new(Blake2bHasher {});
        let method_name = match self {
            MethodName::Ident(i) => i.to_string().to_case(Case::Pascal),
            MethodName::Text(s) => s.value(),
        };

        resolver.method_number(&method_name).expect("invalid method name")
    }
}

impl Parse for MethodName {
    fn parse(input: ParseStream) -> Result<Self> {
        let lookahead = input.lookahead1();

        if lookahead.peek(LitStr) {
            input.parse().map(MethodName::Text)
        } else if lookahead.peek(Ident) {
            input.parse().map(MethodName::Ident)
        } else {
            Err(lookahead.error())
        }
    }
}

#[proc_macro]
pub fn method_hash(input: TokenStream) -> TokenStream {
    let name: MethodName = parse_macro_input!(input);
    let hash = name.hash() as u32;
    // output a u32 literal as our hashed value
    quote!(#hash).into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn string_and_ident_match() {
        let t = trybuild::TestCases::new();
        t.pass("tests/build-success.rs");
    }

    #[test]
    fn empty_names() {
        let t = trybuild::TestCases::new();
        // NOTE: these need to live in a separate directory under `tests`
        // otherwise cargo tries to build them every time and everything breaks
        t.compile_fail("tests/naming/empty-name-string.rs");
        t.compile_fail("tests/naming/missing-name.rs");
    }

    #[test]
    fn bad_names() {
        let t = trybuild::TestCases::new();
        t.compile_fail("tests/naming/illegal-chars.rs");
        t.compile_fail("tests/naming/non-capital-start.rs");
    }
}
