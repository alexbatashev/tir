#![no_main]

use libfuzzer_sys::fuzz_target;

const MAX_INPUT_LEN: usize = 32 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_LEN {
        return;
    }

    if let Ok(input) = std::str::from_utf8(data) {
        let (tokens, errors) = tmdl::lex(input);
        if !errors.is_empty() {
            return;
        }

        let (file, errors) = tmdl::parse(input, &tokens, "<fuzz>");
        if !errors.is_empty() {
            return;
        }

        let Some(file) = file else {
            return;
        };

        let files = [file];
        let _ = tmdl::sema_analyze(&files);
        let _ = tmdl::type_check(&files);
    }
});
