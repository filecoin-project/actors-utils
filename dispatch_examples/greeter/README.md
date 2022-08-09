# Greeter example
A very basic "greeter" actor and an integration test to run it locally. Implements a `Constructor` and a single `Greet` method that takes a string containing a name and returns a greeting.

## To run
`cargo build` to build the actor code
`cargo test` to run it in an integration test (using `fvm_integration_tests`)

Run with `cargo test -- --nocapture` to see the greeting output