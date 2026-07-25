#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, input)) = data.split_first() else {
        return;
    };
    match selector % 3 {
        0 => {
            let _ = focald_secrets::ipc::validate_request_frame(input);
        }
        1 => {
            let _ = focald_secrets::store::validate_encrypted_store(&[0x5a; 32], input);
        }
        _ => {
            let _ = keywrap::unwrap(b"fuzz-password", input);
        }
    }
});
