#![no_main]

use libfuzzer_sys::fuzz_target;
use std::cell::OnceCell;

const MAX_INPUT_LEN: usize = 16 * 1024;

thread_local! {
    static ARM64_CONTEXT: OnceCell<(tir::Context, tir_be_common::AsmParser)> = const { OnceCell::new() };
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_LEN {
        return;
    }

    if let Ok(input) = std::str::from_utf8(data) {
        ARM64_CONTEXT.with(|cell| {
            let (context, parser) = cell.get_or_init(|| {
                let context = tir::Context::with_default_dialects();
                context.register_dialect::<tir_be_common::AsmDialect>();
                context.register_dialect::<arm64::Arm64Dialect>();

                let arm64 = context.find_dialect::<arm64::Arm64Dialect>().unwrap();
                let parser = arm64.get_asm_parser();

                (context, parser)
            });

            let Ok(module) = parser.parse_asm(context, input) else {
                return;
            };
            let _ = tir::Operation::verify(&module, context);
        });
    }
});
