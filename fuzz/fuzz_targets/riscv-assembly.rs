#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        let context = tir::Context::with_default_dialects();
        context.register_dialect::<tir_be_common::AsmDialect>();
        context.register_dialect::<tir_riscv::RiscvDialect>();

        let rv = context.find_dialect::<tir_riscv::RiscvDialect>().unwrap();

        let parser = rv.get_asm_parser();

        let _ = parser.parse_asm(&context, input);
    }
});
