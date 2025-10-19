#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        let (tokens, errors) = tmdl::lex(input);
        if errors.is_empty() {
            let _ = tmdl::parse(input, &tokens, "<fuzz>");
        }
    }
});
