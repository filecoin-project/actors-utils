use fvm_sdk as sdk;

/// Placeholder invoke for testing
#[no_mangle]
pub fn invoke(_params: u32) -> u32 {
    // Conduct method dispatch. Handle input parameters and return data.
    let method_num = sdk::message::method_number();

    match method_num {
        1 => constructor(),
        _ => {
            sdk::vm::abort(
                fvm_shared::error::ExitCode::FIRST_USER_EXIT_CODE,
                Some("sample abort"),
            );
        }
    }
}

fn constructor() -> u32 {
    0_u32
}
