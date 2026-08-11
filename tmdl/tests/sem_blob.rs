//! The sem programs a generated backend embeds live in a binary blob beside the
//! generated Rust, so these properties of behavior lowering are asserted over
//! the decoded blob rather than over the emitted source.

use std::{fs, process};

use tir::sem::{SCALAR_OPS, SemOp, SemPayloadDesc, SymKind, decode_sem_ops};
use tmdl::{Action, Compiler, OutputKind};

/// The generated Rust, the kind table it passes to the decoder, and every
/// program of its sem blob.
struct Generated {
    rust: String,
    kinds: Vec<SymKind>,
    blob: Vec<u8>,
    programs: Vec<String>,
}

impl Generated {
    /// The program the first `extend_sem_bytes` call after `marker` replays.
    fn program_after(&self, marker: &str) -> String {
        let (_, rest) = self.rust.split_once(marker).unwrap();
        let (_, rest) = rest.split_once("SEM_BLOB, ").unwrap();
        let (offset, _) = rest.split_once(')').unwrap();
        render(&decode_sem_ops(
            &self.blob,
            offset.trim().parse().unwrap(),
            &self.kinds,
        ))
    }
}

fn generate(input: &str, dialect: &str) -> Generated {
    let stem = std::path::Path::new(input).file_stem().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "tmdl-sem-blob-{}-{}",
        stem.to_str().unwrap(),
        process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    Compiler::builder()
        .action(Action::EmitRust)
        .dialect(Some(dialect.to_string()))
        .add_input(input)
        .output(OutputKind::File(
            dir.join("generated.rs").to_str().unwrap().to_string(),
        ))
        .build()
        .compile()
        .unwrap();

    let rust = fs::read_to_string(dir.join("generated.rs")).unwrap();
    let kinds = kind_table(&rust);
    let blob = fs::read(dir.join("tmdl_sem.bin")).unwrap();
    let programs = programs(&blob, &kinds);
    Generated {
        rust,
        kinds,
        blob,
        programs,
    }
}

/// The kind table the generated Rust passes to the decoder.
fn kind_table(rust: &str) -> Vec<SymKind> {
    let (_, rest) = rust
        .split_once("static SEM_KINDS: &[tir::sem::SymKind] = &[")
        .unwrap();
    let (table, _) = rest.split_once("];").unwrap();
    table
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| named_kind(entry.trim_start_matches("tir::sem::SymKind::")))
        .collect()
}

/// Codegen names scalar-op kinds by their Rust variant and the rest by their
/// debug name; this inverts both.
fn named_kind(name: &str) -> SymKind {
    if let Some(op) = SCALAR_OPS.iter().find(|op| op.rust == name) {
        return op.kind;
    }
    match name {
        "Symbol" => SymKind::Symbol,
        "Constant" => SymKind::Constant,
        "If" => SymKind::If,
        "ZExt" => SymKind::ZExt,
        "SExt" => SymKind::SExt,
        "Extract" => SymKind::Extract,
        "Concat" => SymKind::Concat,
        "AtomicRmw" => SymKind::AtomicRmw,
        "Fence" => SymKind::Fence,
        "LoadMemory" => SymKind::LoadMemory,
        "StoreMemory" => SymKind::StoreMemory,
        "LoadReserved" => SymKind::LoadReserved,
        "StoreConditional" => SymKind::StoreConditional,
        "StateAssign" => SymKind::StateAssign,
        "StateBlock" => SymKind::StateBlock,
        "StateIf" => SymKind::StateIf,
        "StateStore" => SymKind::StateStore,
        other => panic!("sem kind '{other}' is not known to this test"),
    }
}

/// Every program in the blob, rendered one line each: node kinds in post order
/// with their payloads, then each node's operand indices.
fn programs(blob: &[u8], kinds: &[SymKind]) -> Vec<String> {
    let mut offset = 5;
    let mut rendered = Vec::new();
    while offset < blob.len() {
        let len = u32::from_le_bytes(blob[offset..offset + 4].try_into().unwrap()) as usize;
        rendered.push(render(&decode_sem_ops(blob, offset as u32, kinds)));
        offset += 4 + len;
    }
    rendered
}

fn render(ops: &[SemOp]) -> String {
    let mut out = String::new();
    for op in ops {
        let step = match op {
            SemOp::Node(kind) => format!("{kind:?}"),
            SemOp::Payload(SemPayloadDesc::SymbolId(id)) => format!("#{id}"),
            SemOp::Payload(SemPayloadDesc::Value(value)) => format!("%{value}"),
            SemOp::Payload(SemPayloadDesc::Int { width, value, .. }) => {
                format!("{value}:{width}")
            }
            SemOp::Payload(SemPayloadDesc::Float(value)) => format!("{value}f"),
            SemOp::Typed(width) => format!("<{width}>"),
            SemOp::Edge(parent, child) => format!("{parent}<-{child}"),
        };
        if !out.is_empty() && !matches!(op, SemOp::Payload(_) | SemOp::Typed(_)) {
            out.push(' ');
        }
        out.push_str(&step);
    }
    out
}

#[test]
fn a_let_binding_is_built_once_and_re_read_by_later_statements() {
    let programs = generate("checks/Inputs/lets.tmdl", "test").programs;

    // `let sum = rs1 + rs2` builds the sum from the two operand symbols once.
    assert!(programs.contains(&"Symbol#0 Symbol#1 Add 2<-0 2<-1".to_string()));
    // `rd = sum + sum` reads the binding's symbol instead of rebuilding it.
    assert!(programs.contains(&"Symbol#2 Add 1<-0 1<-0".to_string()));
    // `rd = sext(old, XLEN)` over a bound RMW extends the binding's symbol; the
    // RMW is not re-issued by the write.
    assert!(programs.contains(&"Symbol#2 Symbol#3 SExt 2<-0 2<-1".to_string()));
    // Likewise a bound load: the write adds and extends the binding's symbol.
    assert!(programs.contains(&"Symbol#1 Add 1<-0 1<-0 Symbol#2 SExt 3<-1 3<-2".to_string()));
    // A binding used verbatim as a statement's value is just its symbol.
    assert!(programs.contains(&"Symbol#2".to_string()));
}

#[test]
fn a_const_memory_size_is_folded_to_its_byte_count() {
    let generated = generate("checks/Inputs/param-mem-size.tmdl", "test");

    assert!(
        !generated.kinds.contains(&SymKind::Div),
        "self.XLEN / 8 is not folded"
    );
    assert!(
        generated
            .programs
            .iter()
            .any(|program| program.contains("8:4") && program.contains("LoadMemory"))
    );
    assert!(
        generated
            .programs
            .iter()
            .any(|program| program.contains("8:4") && program.contains("StoreMemory"))
    );
}

#[test]
fn each_atomic_construct_lowers_to_its_dedicated_kind() {
    let generated = generate("checks/Inputs/atomics.tmdl", "test");

    for kind in [
        SymKind::LoadReserved,
        SymKind::StoreConditional,
        SymKind::AtomicRmw,
        SymKind::Fence,
    ] {
        assert!(generated.kinds.contains(&kind), "{kind:?} is not lowered");
    }
    // A plain store with an explicit ordering packs the ordering code into the
    // inert metadata operand (`seq_cst` = 4 -> bits 3:1 -> value 8, width 4).
    assert!(
        generated
            .program_after("impl tir::sem::AsSemExpr for StRelOp")
            .contains("8:4")
    );
}

#[test]
fn a_regnum_guard_compares_the_captured_index_symbol() {
    let generated = generate("checks/Rust/regnum.tmdl", "test");

    assert!(
        generated
            .programs
            .iter()
            .any(|program| program.split(' ').any(|step| step == "Ne"))
    );
}

#[test]
fn flag_reading_rules_compose_the_comparison_they_prove() {
    let generated = generate("checks/Rust/flag_branches.tmdl", "test");

    assert!(
        generated
            .program_after("fn isel_pattern_brancheq_via_cmp(")
            .contains("Eq")
    );
    assert!(
        generated
            .program_after("fn isel_pattern_branchlt_via_cmp(")
            .contains("Lt")
    );
    assert!(
        generated
            .program_after("fn isel_pattern_selecteq_via_cmp(")
            .contains("If")
    );
}
