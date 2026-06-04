use crate::Terminator;
use crate::operation;

use crate as tir;

operation! {
    ModuleOp {
        name: "module",
        dialect: "builtin",
        regions: R {
            body: Region {
                single_block: true,
            }
        }
    }
}

operation! {
    ModuleEndOp {
        name: "module_end",
        dialect: "builtin",
        interfaces: [Terminator],
    }
}

impl Terminator for ModuleEndOp {}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, Operation,
        builtin::{ModuleOp, ops},
        parse::ir::parse_ir,
    };

    #[test]
    fn module_creation() {
        let context = Context::with_default_dialects();
        let m = ops::module(&context, None).build();

        let mut builder = IRBuilder::new(m.body());
        builder.insert(ops::module_end(&context).build());

        assert_eq!(m.regions().len(), 1);
        assert_eq!(m.body().iter(context.clone()).len(), 1);

        let mut buf = String::new();
        let mut f = IRFormatter::new(&mut buf);
        m.print(&mut f).expect("ok");
        assert!(!buf.is_empty());

        let new_op = parse_ir::<ModuleOp>(&context, &buf).expect("Failed to parse constructed op");
        let mut new_buf = String::new();
        let mut f = IRFormatter::new(&mut new_buf);
        new_op.print(&mut f).expect("ok");
        assert_eq!(buf, new_buf);
    }
}
