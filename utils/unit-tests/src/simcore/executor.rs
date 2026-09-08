use tir::utils::APInt;
use tir::Context;
use tir_sim::{Executor, MemAccess, MemAccessKind};

use super::support::riscv_program;

#[test]
fn mem_trace_records_loads_and_stores_parallel_to_trace() {
    use tir::backend::MachineContext;

    let context = Context::with_default_dialects();
    // Reverse declaration order: `first` executes at 0x8000_0000 and falls
    // through to `last` at 0x8000_000c after three instructions.
    let program = riscv_program(
        &context,
        "
            .global last
            last:
              add x0, x0, x0
            .global first
            first:
              lw  x2, 0(x1)
              sw  x2, 4(x1)
              add x3, x2, x2
        ",
        "first",
    );

    let base = 0x8000_0000;
    let data = base + 0x100;
    let mut executor = Executor::new_at(4096, base);
    executor.enable_trace_recording();
    MachineContext::write_register(&mut executor, "GPR", 1, APInt::new(64, data)).unwrap();
    MachineContext::write_memory(&mut executor, data, 4, 0x1234_5678).unwrap();
    executor.load(program).unwrap();
    executor.run(0x8000_000c, 10).unwrap();

    assert_eq!(executor.trace().len(), 3);
    assert_eq!(executor.mem_trace().len(), executor.trace().len());
    assert_eq!(
        executor.mem_trace()[0],
        vec![MemAccess {
            addr: data,
            size: 4,
            is_write: false,
            ..Default::default()
        }]
    );
    assert_eq!(
        executor.mem_trace()[1],
        vec![MemAccess {
            addr: data + 4,
            size: 4,
            is_write: true,
            ..Default::default()
        }]
    );
    assert!(executor.mem_trace()[2].is_empty(), "add touches no memory");
}

#[test]
fn pc_advances_by_each_instructions_encoded_length() {
    // A compressed instruction is two bytes and an uncompressed one four, so
    // the program counter follows what each instruction encodes to rather than
    // one width per opcode.
    let context = Context::with_default_dialects();
    let program = riscv_program(
        &context,
        "
            .global last
            last:
              add x0, x0, x0
            .global first
            first:
              c.addi a0, 1
              add    a1, a0, a0
              c.addi a0, 1
        ",
        "first",
    );

    let base = 0x8000_0000;
    let mut executor = Executor::new_at(4096, base);
    executor.enable_trace_recording();
    executor.load(program).unwrap();
    executor.run(base + 8, 10).unwrap();

    let pcs: Vec<u64> = executor.trace().iter().map(|(_, pc)| *pc).collect();
    assert_eq!(pcs, vec![base, base + 2, base + 6]);
}

#[test]
fn mem_trace_records_atomic_kinds() {
    let context = Context::with_default_dialects();
    let program = riscv_program(
        &context,
        "
            .global last
            last:
              add x0, x0, x0
            .global first
            first:
              lr.w      t0, (a0)
              sc.w      t1, t0, (a0)
              amoadd.w  t2, t0, (a0)
        ",
        "first",
    );

    let base = 0x8000_0000;
    let mut executor = Executor::new_at(4096, base);
    executor.enable_trace_recording();
    tir::backend::MachineContext::write_register(&mut executor, "GPR", 10, APInt::new(64, base))
        .unwrap();
    executor.load(program).unwrap();
    executor.run(0x8000_000c, 10).unwrap();

    let kinds: Vec<_> = executor
        .mem_trace()
        .iter()
        .flatten()
        .map(|access| access.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            MemAccessKind::LoadReserved,
            MemAccessKind::StoreConditional { success: true },
            // The AMO records its read-modify-write plus the sc's store.
            MemAccessKind::AtomicRmw,
        ]
    );
}

#[test]
fn fence_records_its_kind() {
    let context = Context::with_default_dialects();
    let program = riscv_program(
        &context,
        "
            .global last
            last:
              add x0, x0, x0
            .global first
            first:
              fence 3, 3
              fence.i
        ",
        "first",
    );

    let mut executor = Executor::new_at(4096, 0x8000_0000);
    executor.enable_trace_recording();
    executor.load(program).unwrap();
    executor.run(0x8000_0008, 10).unwrap();

    let kinds: Vec<_> = executor
        .mem_trace()
        .iter()
        .flatten()
        .map(|access| access.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            MemAccessKind::Fence {
                pred: 0b0011,
                succ: 0b0011,
                ifence: false,
            },
            MemAccessKind::Fence {
                pred: 0,
                succ: 0,
                ifence: true,
            },
        ]
    );
}

#[test]
fn exception_handler_controls_run_outcome() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let context = Context::with_default_dialects();
    let program = riscv_program(
        &context,
        "
            .global last
            last:
              add x0, x0, x0
            .global first
            first:
              ecall
              addi x1, x0, 7
              ebreak
              addi x2, x0, 9
        ",
        "first",
    );

    let traps = Rc::new(RefCell::new(Vec::new()));
    let seen = traps.clone();
    let mut executor = Executor::new(4096);
    executor.set_exception_handler(Box::new(move |_executor, cause, pc| {
        seen.borrow_mut().push((cause, pc));
        // Resume after the ecall, stop at the ebreak.
        if cause == 11 {
            tir_sim::ExceptionAction::Continue
        } else {
            tir_sim::ExceptionAction::Halt
        }
    }));
    executor.load(program).unwrap();
    executor.run(0x8000_0010, 10).unwrap();

    assert!(executor.halted());
    assert_eq!(
        *traps.borrow(),
        vec![(11, 0x8000_0000), (3, 0x8000_0008)],
        "handler saw the ecall and the ebreak with their PCs"
    );
    let reg = |idx| {
        tir::backend::MachineContext::read_register(&executor, "GPR", idx)
            .unwrap()
            .to_u64()
    };
    assert_eq!(reg(1), 7, "execution resumed after the ecall");
    assert_eq!(reg(2), 0, "the halt stopped execution at the ebreak");
}

/// `bl` writes two destinations: the link register (x30 = pc + 4) and PC. Both
/// used to be silently dropped, because the multi-assignment behaviors only
/// ever emitted one write.
#[test]
fn arm64_branch_link_writes_the_link_register_and_pc() {
    use tir::attributes::AttributeValue;
    use tir::backend::{MachineContext, MachineInstruction};
    use tir::Operation;

    let context = Context::with_default_dialects();
    context.register_dialect::<tir::backend::AsmDialect>();
    context.register_dialect::<tir_arm64::Arm64Dialect>();

    let mut ex = Executor::new(64);
    MachineContext::write_pc(&mut ex, 0x2000);
    let bl = tir_arm64::BranchLinkOpBuilder::new(&context)
        .attr("imm", AttributeValue::Int(3))
        .build();
    let mi = context
        .get_op(bl.id())
        .as_interface::<dyn MachineInstruction>()
        .expect("bl is a machine instruction");
    mi.execute(&mut ex).expect("bl executes");

    let x30 = MachineContext::read_register(&ex, "GPR", 30)
        .unwrap()
        .to_u64();
    assert_eq!(
        x30, 0x2004,
        "link register holds the return address (pc + 4)"
    );
    assert_eq!(
        MachineContext::read_pc(&ex),
        0x2000 + (3 << 2),
        "pc takes the branch target"
    );
}
