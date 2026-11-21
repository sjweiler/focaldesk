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
            let Some((key, ciphertext)) = input.split_first_chunk::<32>() else {
                return;
            };
            let _ = focald_secrets::store::validate_encrypted_store(key, ciphertext);
        }
        _ => {
            let Some((&length, input)) = input.split_first() else {
                return;
            };
            let Some((password, wrapped)) = input.split_at_checked(usize::from(length)) else {
                return;
            };
            let _ = keywrap::unwrap(password, wrapped);
        }
    }
});
