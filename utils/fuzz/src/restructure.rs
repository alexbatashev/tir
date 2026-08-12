//! Random control-flow graphs through the `restructure` pass.
//!
//! Every generated function is restructured and verified. A generated function
//! whose graph is acyclic is also *executed*: the original CFG and the
//! restructured program (lowered back to a CFG by `scf-to-cfg`) are compiled by
//! the JIT and must agree on every argument pair. Cyclic graphs — including the
//! irreducible ones the generator makes — are checked for verification only,
//! since a random loop need not terminate.

use std::sync::Arc;

use tir::builtin::ops as b;
use tir::builtin::{FuncOp, IntegerType, ModuleOp};
use tir::{Block, Context, IRFormatter, Operation, PassManager};

/// The arguments an executed comparison is run on.
const ARGUMENTS: [(i64, i64); 5] = [(0, 0), (1, 2), (-3, 7), (11, -5), (i64::MAX, 3)];

pub fn check(data: &[u8]) {
    let Some(program) = Program::generate(data) else {
        return;
    };

    let context = Context::with_default_dialects();
    let module = program.build(&context);
    let source = render(&context, &module);

    restructure(&context, &module)
        .unwrap_or_else(|error| panic!("restructure failed: {error}\n{source}"));
    tir::verify_op_tree(&context, module.id())
        .unwrap_or_else(|error| panic!("restructured IR does not verify: {error}\n{source}"));
    assert!(
        !has_branches(&context, &module),
        "restructured IR still holds a CFG:\n{}",
        render(&context, &module)
    );

    if program.acyclic {
        compare_execution(&program, &source, &render(&context, &module));
    }
}

fn restructure(context: &Context, module: &ModuleOp) -> Result<(), tir::PassError> {
    let mut passes = PassManager::new();
    passes
        .nest::<FuncOp>()
        .add_pass(tir::passes::RestructurePass::new());
    passes.run(context, context.get_op(module.id()))
}

/// A block ends with a branch only where restructuring left control flow
/// behind: the pass must leave a single block of structured operations.
fn has_branches(context: &Context, module: &ModuleOp) -> bool {
    module.body().iter(context.clone()).any(|op| {
        op.clone().as_op::<FuncOp>().is_some_and(|func| {
            func.regions()
                .any(|region| region.iter(context.clone()).len() > 1)
        })
    })
}

fn render(context: &Context, module: &ModuleOp) -> String {
    let mut rendered = String::new();
    let mut formatter = IRFormatter::new(&mut rendered);
    tir::print_ir(module, context, &mut formatter).expect("print");
    rendered
}

/// Lower the structured program back to a CFG, which is the form the backend
/// takes until destruction moves into emission.
fn lower_to_cfg(context: &Context, module: &ModuleOp) {
    let mut passes = PassManager::new();
    passes
        .nest::<FuncOp>()
        .add_pass(tir::passes::ScfToCfgPass::new());
    passes
        .run(context, context.get_op(module.id()))
        .expect("scf-to-cfg");
}

/// Build the program twice — the pipeline consumes the module it compiles —
/// and check that restructuring left what it computes alone.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn compare_execution(program: &Program, before: &str, after: &str) {
    let jit = tir_jit::Jit::host().expect("host target");
    let original = jit.context().expect("host context");
    let original_module = program.build(&original);
    let Ok(original) = jit.compile_module(&original, &original_module) else {
        return;
    };

    let context = jit.context().expect("host context");
    let module = program.build(&context);
    restructure(&context, &module).expect("restructure");
    lower_to_cfg(&context, &module);
    let restructured = jit
        .compile_module(&context, &module)
        .unwrap_or_else(|error| panic!("restructured program does not compile: {error}\n{after}"));

    let original: extern "C" fn(i64, i64) -> i64 =
        unsafe { original.get("f") }.expect("f in the original");
    let restructured: extern "C" fn(i64, i64) -> i64 =
        unsafe { restructured.get("f") }.expect("f in the restructured program");
    for (left, right) in ARGUMENTS {
        assert_eq!(
            original(left, right),
            restructured(left, right),
            "restructuring changed what f({left}, {right}) computes\n{before}\n{after}"
        );
    }
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn compare_execution(_program: &Program, _before: &str, _after: &str) {}

/// A generated function: `blocks` basic blocks, each computing one value from
/// its own argument and leaving through the terminator the fuzz input chose.
struct Program {
    blocks: Vec<Vertex>,
    acyclic: bool,
}

struct Vertex {
    operation: Arith,
    terminator: Terminator,
}

#[derive(Clone, Copy)]
enum Arith {
    Add,
    Sub,
    Mul,
}

enum Terminator {
    Jump(usize),
    Branch(usize, usize),
    Return,
}

impl Program {
    fn generate(data: &[u8]) -> Option<Program> {
        let mut bytes = Bytes::new(data)?;
        let count = 2 + bytes.next() as usize % 7;
        let acyclic = bytes.next() % 2 == 0;
        let blocks = (0..count)
            .map(|index| {
                let operation = match bytes.next() % 3 {
                    0 => Arith::Add,
                    1 => Arith::Sub,
                    _ => Arith::Mul,
                };
                let last = index + 1 == count;
                let terminator = match (last, bytes.next() % 4) {
                    (true, _) | (_, 0) => Terminator::Return,
                    (_, 1) => Terminator::Jump(Self::target(&mut bytes, index, count, acyclic)),
                    _ => Terminator::Branch(
                        Self::target(&mut bytes, index, count, acyclic),
                        Self::target(&mut bytes, index, count, acyclic),
                    ),
                };
                Vertex {
                    operation,
                    terminator,
                }
            })
            .collect();
        Some(Program { blocks, acyclic })
    }

    /// An acyclic program only ever jumps forward, so executing it terminates.
    fn target(bytes: &mut Bytes<'_>, from: usize, count: usize, acyclic: bool) -> usize {
        let choice = bytes.next() as usize;
        match acyclic {
            true => from + 1 + choice % (count - from - 1).max(1),
            false => choice % count,
        }
    }

    /// The generated function, built through the IR API: the text form cannot
    /// express a forward branch to a block that later declares arguments.
    fn build(&self, context: &Context) -> ModuleOp {
        let integer = IntegerType::new(context, 64);
        let boolean = IntegerType::new(context, 1);
        let parameters = [
            context.create_value(integer, None),
            context.create_value(integer, None),
        ];
        let arguments = [parameters[0].id(), parameters[1].id()];
        let region = context.create_region();
        let entry = context.create_block(parameters.to_vec());
        region.add_block(entry.id());
        let blocks = self
            .blocks
            .iter()
            .map(|_| {
                let argument = context.create_value(integer, None);
                let block = context.create_block(vec![argument]);
                region.add_block(block.id());
                block
            })
            .collect::<Vec<Arc<Block>>>();

        let seed = entry
            .append_op(b::addi(context, arguments[0], arguments[1], integer).build())
            .result();
        entry.append_op(b::br(context, vec![seed], blocks[0].id()).build());

        for (index, specification) in self.blocks.iter().enumerate() {
            let block = &blocks[index];
            let argument = block.arguments()[0].id();
            let arithmetic = match specification.operation {
                Arith::Add => b::addi(context, argument, seed, integer).build().id(),
                Arith::Sub => b::subi(context, argument, seed, integer).build().id(),
                Arith::Mul => b::muli(context, argument, seed, integer).build().id(),
            };
            block.append(arithmetic);
            let value = context.get_op(arithmetic).results[0];
            match specification.terminator {
                Terminator::Return => {
                    block.append_op(b::r#return(context, value).build());
                }
                Terminator::Jump(target) => {
                    block.append_op(b::br(context, vec![value], blocks[target].id()).build());
                }
                Terminator::Branch(left, right) => {
                    let condition = block
                        .append_op(
                            b::CmpIOpBuilder::new(context)
                                .lhs(value)
                                .rhs(seed)
                                .predicate("slt")
                                .result_type(boolean)
                                .build(),
                        )
                        .result();
                    block.append_op(
                        b::cond_br(
                            context,
                            condition,
                            vec![value],
                            vec![argument],
                            blocks[left].id(),
                            blocks[right].id(),
                        )
                        .build(),
                    );
                }
            }
        }

        let func = b::func(context, "f", integer, Some(region.id())).build();
        let module = b::module(context, None).build();
        module.body().append(func.id());
        module.body().append(b::module_end(context).build().id());
        module
    }
}

/// The fuzz input read as an endless stream of choices.
struct Bytes<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        (!data.is_empty()).then_some(Self { data, position: 0 })
    }

    fn next(&mut self) -> u8 {
        let byte = self.data[self.position % self.data.len()];
        self.position += 1;
        byte.wrapping_add((self.position / self.data.len()) as u8)
    }
}

#[cfg(test)]
mod tests {
    /// A bounded smoke campaign: enough shapes to cover branches, joins,
    /// dispatch and irreducible loops.
    #[test]
    fn five_hundred_random_graphs_restructure() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..500 {
            let mut input = Vec::new();
            for _ in 0..16 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                input.push(state as u8);
            }
            super::check(&input);
        }
    }
}
