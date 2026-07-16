//! Lowers the C [`crate::ast`] to TIR using the `builtin` and `ptr` dialects.
//!
//! The lowering is intentionally memory-based (the unoptimised, "no memory
//! SSA" shape a C frontend emits before any mem2reg pass): every parameter and
//! local lives in a stack slot produced by `ptr.alloca`, reads become
//! `ptr.load` and writes become `ptr.store`. Arithmetic uses the `builtin`
//! integer ops; C-only literals and variadic markers use the local `cir` dialect.

use std::collections::{BTreeMap, HashMap};

use tir::attributes::AttributeValue;
use tir::builtin::{IntegerType, ModuleOp, TokenType, UnitType, ops as b};
use tir::graph::{Dag, NodeId};
use tir::ptr::{PtrType, ops as p};
use tir::{Context, IRBuilder, Operand, Operation, TypeId, ValueId};

use crate::ast::*;
use crate::cir::{self, StructType, VarArgsType};
use crate::diagnostics::{Diagnostic, EmptyTranslationUnit, UnsupportedConstruct};
use crate::sema::{EntityId, QualType, TypeKind, TypedAst};

/// A local variable: the pointer to its stack slot and the slot's element type.
#[derive(Clone, Copy)]
struct Slot {
    ptr: ValueId,
    elem: TypeId,
}

#[derive(Clone, Copy)]
enum LoweredExpr {
    Value(ValueId),
    Address { ptr: ValueId, elem: TypeId },
}

struct FnCodegen<'a> {
    context: &'a Context,
    typed: &'a TypedAst,
    ast: &'a Ast,
    builder: IRBuilder,
    locals: HashMap<EntityId, Slot>,
    signatures: &'a HashMap<EntityId, Signature>,
    loop_scopes: Vec<ValueId>,
    terminated: bool,
    /// Scratch holding the lowered SSA value of each node in the expression
    /// subtree currently being lowered, indexed by `node.index() - base`. Reused
    /// across expressions to avoid reallocating.
    values: Vec<LoweredExpr>,
}

#[derive(Clone)]
struct Signature {
    ret: TypeId,
    args: Vec<TypeId>,
}

/// Lower a translation unit into a `builtin.module` in `context`.
pub fn codegen(context: &Context, typed: &TypedAst) -> Result<ModuleOp, Diagnostic> {
    let ast = typed.ast();
    let module = b::module(context, None).build();
    let mut module_builder = IRBuilder::new(module.body());

    let root = ast.root().ok_or_else(EmptyTranslationUnit::new)?;
    let mut signatures = HashMap::new();
    for item in ast.children(root) {
        match ast.get_node(item).kind {
            AstKind::Prototype | AstKind::Function => {
                let (entity, sig) = lower_signature(context, typed, item)?;
                signatures.insert(entity, sig);
            }
            AstKind::DeclGroup
            | AstKind::RecordDecl
            | AstKind::Typedef
            | AstKind::Global
            | AstKind::Attribute => {}
            _ => return Err(unsupported(ast, item, "top-level item".to_string())),
        }
    }

    for record in typed.records() {
        let fields = record
            .fields
            .iter()
            .map(|field| {
                AttributeValue::Dict(BTreeMap::from([
                    ("name".to_string(), AttributeValue::Str(field.name.clone())),
                    (
                        "type".to_string(),
                        AttributeValue::Type(lower_type(context, typed, field.ty)),
                    ),
                    ("offset".to_string(), AttributeValue::UInt(field.offset)),
                ]))
            })
            .collect();
        module_builder.insert(
            cir::DefineStructOpBuilder::new(context)
                .attr("sym_name", AttributeValue::Str(record.name.clone()))
                .attr("fields", AttributeValue::Array(fields))
                .attr("size", AttributeValue::UInt(record.size))
                .attr("align", AttributeValue::UInt(record.align))
                .build(),
        );
    }

    for item in ast.children(root) {
        match ast.get_node(item).kind {
            AstKind::Prototype => {
                let AstLeaf::Function { name, .. } = ast.get_leaf_data(item).unwrap() else {
                    unreachable!("prototype node carries a function payload");
                };
                let entity = node_entity(typed, item);
                let sig = signatures.get(&entity).unwrap();
                module_builder.insert(b::declare_op(context, name, sig.ret, &sig.args));
            }
            AstKind::Function => {
                let func_op = lower_function(context, typed, item, &signatures)?;
                module_builder.insert(func_op);
            }
            AstKind::DeclGroup
            | AstKind::RecordDecl
            | AstKind::Typedef
            | AstKind::Global
            | AstKind::Attribute => {}
            _ => unreachable!("top-level item was checked before emission"),
        }
    }
    module_builder.insert(b::module_end(context).build());
    Ok(module)
}

/// A construct the parser accepts but codegen does not lower yet.
fn unsupported(ast: &Ast, node: NodeId, what: String) -> Diagnostic {
    UnsupportedConstruct::new(ast.get_node(node).span, what).into()
}

fn lower_type(context: &Context, typed: &TypedAst, ty: QualType) -> TypeId {
    match typed.types().kind(ty) {
        TypeKind::Void => UnitType::new(context),
        TypeKind::Integer(_) => IntegerType::new(context, typed.integer_width(ty).unwrap()),
        TypeKind::Pointer(pointee) => {
            if matches!(typed.types().kind(*pointee), TypeKind::Record(_)) {
                PtrType::opaque(context)
            } else {
                PtrType::typed(context, lower_type(context, typed, *pointee))
            }
        }
        TypeKind::Array(pointee, _) => {
            PtrType::typed(context, lower_type(context, typed, *pointee))
        }
        TypeKind::Enum(_) => IntegerType::new(context, 32),
        TypeKind::Error
        | TypeKind::Float
        | TypeKind::Double
        | TypeKind::LongDouble
        | TypeKind::Function { .. } => IntegerType::new(context, 64),
        TypeKind::Record(id) => StructType::new(context, &typed.record(*id).unwrap().name),
    }
}

fn source_type_layout(typed: &TypedAst, ty: QualType) -> (u64, u64) {
    match typed.types().kind(ty) {
        TypeKind::Integer(_) => {
            let size = u64::from(typed.integer_width(ty).unwrap() / 8);
            (size, size)
        }
        TypeKind::Pointer(_) => {
            let size = u64::from(typed.target().pointer_width() / 8);
            (size, size)
        }
        TypeKind::Array(element, Some(length)) => {
            let (size, align) = source_type_layout(typed, *element);
            (size * length, align)
        }
        TypeKind::Record(id) => {
            let record = typed.record(*id).unwrap();
            (record.size, record.align)
        }
        TypeKind::Float => (4, 4),
        TypeKind::Double => (8, 8),
        TypeKind::LongDouble => (16, 16),
        TypeKind::Enum(_) => (4, 4),
        _ => (1, 1),
    }
}

fn node_type(typed: &TypedAst, node: NodeId) -> QualType {
    typed
        .ast()
        .get_annotation(node)
        .and_then(|info| info.ty)
        .expect("semantic analysis annotates codegen nodes")
}

fn node_entity(typed: &TypedAst, node: NodeId) -> EntityId {
    typed
        .ast()
        .get_annotation(node)
        .and_then(|info| info.entity)
        .expect("semantic analysis resolves codegen names")
}

fn lower_signature(
    context: &Context,
    typed: &TypedAst,
    item: NodeId,
) -> Result<(EntityId, Signature), Diagnostic> {
    let ast = typed.ast();
    let AstLeaf::Function { .. } = ast.get_leaf_data(item).unwrap() else {
        unreachable!("function-like node carries a function payload");
    };
    let mut args = Vec::new();
    for child in ast.children(item) {
        match ast.get_node(child).kind {
            AstKind::Param => {
                args.push(lower_type(context, typed, node_type(typed, child)));
            }
            AstKind::VarArgs => args.push(VarArgsType::new(context)),
            _ => break,
        }
    }
    Ok((
        node_entity(typed, item),
        Signature {
            ret: match typed.types().kind(node_type(typed, item)) {
                TypeKind::Function { ret, .. } => lower_type(context, typed, *ret),
                _ => unreachable!("function node has function semantic type"),
            },
            args,
        },
    ))
}

fn lower_function(
    context: &Context,
    typed: &TypedAst,
    func: NodeId,
    signatures: &HashMap<EntityId, Signature>,
) -> Result<impl Operation, Diagnostic> {
    let ast = typed.ast();
    let AstLeaf::Function { name, .. } = ast.get_leaf_data(func).unwrap() else {
        unreachable!("function node carries a function payload");
    };
    let ret_ty = match typed.types().kind(node_type(typed, func)) {
        TypeKind::Function { ret, .. } => lower_type(context, typed, *ret),
        _ => unreachable!("function node has function semantic type"),
    };

    // Entry block arguments carry the incoming parameter values; parameters are
    // the function node's leading children.
    let mut param_values = Vec::new();
    for param in ast
        .children(func)
        .take_while(|&c| matches!(ast.get_node(c).kind, AstKind::Param))
    {
        param_values
            .push(context.create_value(lower_type(context, typed, node_type(typed, param)), None));
    }
    let param_ids: Vec<ValueId> = param_values.iter().map(|v| v.id()).collect();

    let region = context.create_region();
    let block = context.create_block(param_values);
    region.add_block(block.id());

    let func_op = b::func(context, name.as_str(), ret_ty, Some(region.id())).build();

    let mut cg = FnCodegen {
        context,
        typed,
        ast,
        builder: IRBuilder::new(func_op.body()),
        locals: HashMap::new(),
        signatures,
        loop_scopes: Vec::new(),
        terminated: false,
        values: Vec::new(),
    };
    cg.lower_body(func, &param_ids)?;

    Ok(func_op)
}

impl FnCodegen<'_> {
    fn alloca(&mut self, elem: TypeId, size: u64, align: u64) -> Slot {
        let ptr_ty = PtrType::typed(self.context, elem);
        let op = self
            .builder
            .insert(p::alloca(self.context, size, align, ptr_ty).build());
        Slot {
            ptr: op.result(),
            elem,
        }
    }

    fn apply_conversions(&mut self, node: NodeId, mut value: ValueId) -> ValueId {
        let semantics = self.ast.get_annotation(node).unwrap();
        let mut source = semantics.ty.unwrap();
        for &target in &semantics.conversions {
            if let (Some(source_width), Some(target_width)) = (
                self.typed.integer_width(source),
                self.typed.integer_width(target),
            ) {
                let target_ty = lower_type(self.context, self.typed, target);
                if source_width < target_width {
                    value = if self.typed.integer_is_signed(source).unwrap() {
                        self.builder
                            .insert(b::extsi(self.context, value, target_ty).build())
                            .result()
                    } else {
                        self.builder
                            .insert(b::extui(self.context, value, target_ty).build())
                            .result()
                    };
                } else if source_width > target_width {
                    value = self
                        .builder
                        .insert(b::trunci(self.context, value, target_ty).build())
                        .result();
                }
            }
            source = target;
        }
        value
    }

    /// Lower a function: spill parameters into stack slots, then lower each body
    /// statement in source order (statement order is a side-effect ordering, so it
    /// stays top-down; only the expressions within use the post-order iterator).
    fn lower_body(&mut self, func: NodeId, param_ids: &[ValueId]) -> Result<(), Diagnostic> {
        let ast = self.ast;

        let mut idx = 0;
        for param in ast
            .children(func)
            .take_while(|&c| matches!(ast.get_node(c).kind, AstKind::Param))
        {
            let AstLeaf::Param { .. } = ast.get_leaf_data(param).unwrap() else {
                unreachable!("param node carries a param payload");
            };
            let source_ty = node_type(self.typed, param);
            let elem = lower_type(self.context, self.typed, source_ty);
            let (size, align) = source_type_layout(self.typed, source_ty);
            let slot = self.alloca(elem, size, align);
            self.builder
                .insert(p::store(self.context, param_ids[idx], slot.ptr).build());
            idx += 1;
            self.locals.insert(node_entity(self.typed, param), slot);
        }

        for stmt in ast.children(func).skip(idx) {
            self.lower_stmt(stmt)?;
            if self.terminated {
                break;
            }
        }

        Ok(())
    }

    fn lower_stmt(&mut self, stmt: NodeId) -> Result<(), Diagnostic> {
        let ast = self.ast;
        match ast.get_node(stmt).kind {
            AstKind::Decl => {
                let AstLeaf::Decl { .. } = ast.get_leaf_data(stmt).unwrap() else {
                    unreachable!("decl node carries a decl payload");
                };
                let source_ty = node_type(self.typed, stmt);
                let elem = lower_type(self.context, self.typed, source_ty);
                let (size, align) = source_type_layout(self.typed, source_ty);
                let slot = self.alloca(elem, size, align);
                if let Some(init) = ast.children(stmt).next() {
                    let value = self.lower_expr(init)?;
                    self.builder
                        .insert(p::store(self.context, value, slot.ptr).build());
                }
                self.locals.insert(node_entity(self.typed, stmt), slot);
                Ok(())
            }
            AstKind::Assign => {
                let AstLeaf::Assign(_) = ast.get_leaf_data(stmt).unwrap() else {
                    unreachable!("assign node carries an assign payload");
                };
                let slot = self.locals[&node_entity(self.typed, stmt)];
                let value = ast.children(stmt).next().unwrap();
                if let TypeKind::Record(id) = self.typed.types().kind(node_type(self.typed, stmt)) {
                    let LoweredExpr::Address { ptr: source, .. } = self.lower_expr_value(value)?
                    else {
                        return Err(unsupported(
                            ast,
                            stmt,
                            "non-addressable struct source".to_string(),
                        ));
                    };
                    self.builder.insert(
                        cir::ops::copy_struct(
                            self.context,
                            slot.ptr,
                            source,
                            self.typed.record(*id).unwrap().name.as_str(),
                        )
                        .build(),
                    );
                } else {
                    let v = self.lower_expr(value)?;
                    self.builder
                        .insert(p::store(self.context, v, slot.ptr).build());
                }
                Ok(())
            }
            AstKind::Return => {
                let operand = match ast.children(stmt).next() {
                    Some(e) => Operand::from(self.lower_expr(e)?),
                    None => Operand::none(),
                };
                self.builder
                    .insert(b::r#return(self.context, operand).build());
                self.terminated = true;
                Ok(())
            }
            AstKind::ExprStmt => {
                if let Some(expr) = ast.children(stmt).next() {
                    self.lower_expr(expr)?;
                }
                Ok(())
            }
            AstKind::Block => {
                for child in ast.children(stmt) {
                    self.lower_stmt(child)?;
                    if self.terminated {
                        break;
                    }
                }
                Ok(())
            }
            AstKind::While => {
                let mut children = ast.children(stmt);
                let condition = children.next().unwrap();
                let body = children.next().unwrap();
                let scope = self
                    .context
                    .create_value(TokenType::new(self.context), None);

                let condition_region = self.context.create_region();
                let condition_block = self.context.create_block(vec![]);
                condition_region.add_block(condition_block.id());
                self.in_block(condition_block, |cg| {
                    let value = cg.lower_condition(condition)?;
                    cg.builder
                        .insert(cir::ops::condition(cg.context, value).build());
                    Ok(())
                })?;

                let body_region = self.context.create_region();
                let body_block = self.context.create_block(vec![scope.clone()]);
                body_region.add_block(body_block.id());
                self.loop_scopes.push(scope.id());
                self.in_block(body_block.clone(), |cg| {
                    cg.lower_stmt(body)?;
                    cg.ensure_cir_yield(body_block);
                    Ok(())
                })?;
                self.loop_scopes.pop();

                self.builder.insert(
                    cir::ops::r#while(
                        self.context,
                        Some(condition_region.id()),
                        Some(body_region.id()),
                    )
                    .build(),
                );
                Ok(())
            }
            AstKind::DoWhile => {
                let mut children = ast.children(stmt);
                let body = children.next().unwrap();
                let condition = children.next().unwrap();

                let scope = self
                    .context
                    .create_value(TokenType::new(self.context), None);
                let body_region = self.context.create_region();
                let body_block = self.context.create_block(vec![scope.clone()]);
                body_region.add_block(body_block.id());
                self.loop_scopes.push(scope.id());
                self.in_block(body_block.clone(), |cg| {
                    cg.lower_stmt(body)?;
                    cg.ensure_cir_yield(body_block);
                    Ok(())
                })?;
                self.loop_scopes.pop();

                let condition_region = self.context.create_region();
                let condition_block = self.context.create_block(vec![]);
                condition_region.add_block(condition_block.id());
                self.in_block(condition_block, |cg| {
                    let value = cg.lower_condition(condition)?;
                    cg.builder
                        .insert(cir::ops::condition(cg.context, value).build());
                    Ok(())
                })?;

                self.builder.insert(
                    cir::ops::r#do(
                        self.context,
                        Some(body_region.id()),
                        Some(condition_region.id()),
                    )
                    .build(),
                );
                Ok(())
            }
            AstKind::For => {
                let children = ast.children(stmt).collect::<Vec<_>>();
                let [init, condition, step, body] = children.as_slice() else {
                    unreachable!("for statement has four children");
                };
                if ast.get_node(*init).kind != AstKind::Empty {
                    self.lower_stmt(*init)?;
                }
                let scope = self
                    .context
                    .create_value(TokenType::new(self.context), None);

                let condition_region = self.context.create_region();
                let condition_block = self.context.create_block(vec![]);
                condition_region.add_block(condition_block.id());
                self.in_block(condition_block, |cg| {
                    let value = if ast.get_node(*condition).kind == AstKind::Empty {
                        cg.builder
                            .insert(
                                b::constant(cg.context, 1, IntegerType::new(cg.context, 1)).build(),
                            )
                            .result()
                    } else {
                        cg.lower_condition(*condition)?
                    };
                    cg.builder
                        .insert(cir::ops::condition(cg.context, value).build());
                    Ok(())
                })?;

                let body_region = self.context.create_region();
                let body_block = self.context.create_block(vec![scope.clone()]);
                body_region.add_block(body_block.id());
                self.loop_scopes.push(scope.id());
                self.in_block(body_block.clone(), |cg| {
                    cg.lower_stmt(*body)?;
                    cg.ensure_cir_yield(body_block);
                    Ok(())
                })?;
                self.loop_scopes.pop();

                let step_region = self.context.create_region();
                let step_block = self.context.create_block(vec![]);
                step_region.add_block(step_block.id());
                self.in_block(step_block.clone(), |cg| {
                    if ast.get_node(*step).kind != AstKind::Empty {
                        cg.lower_stmt(*step)?;
                    }
                    cg.ensure_cir_yield(step_block);
                    Ok(())
                })?;

                self.builder.insert(
                    cir::ops::r#for(
                        self.context,
                        Some(condition_region.id()),
                        Some(body_region.id()),
                        Some(step_region.id()),
                    )
                    .build(),
                );
                Ok(())
            }
            AstKind::If => {
                let mut children = ast.children(stmt);
                let condition = children.next().unwrap();
                let then_stmt = children.next().unwrap();
                let else_stmt = children.next();
                let condition = self.lower_condition(condition)?;

                let then_region = self.context.create_region();
                let then_block = self.context.create_block(vec![]);
                then_region.add_block(then_block.id());
                self.in_block(then_block.clone(), |cg| {
                    cg.lower_stmt(then_stmt)?;
                    cg.ensure_cir_yield(then_block);
                    Ok(())
                })?;

                let else_region = self.context.create_region();
                let else_block = self.context.create_block(vec![]);
                else_region.add_block(else_block.id());
                self.in_block(else_block.clone(), |cg| {
                    if let Some(else_stmt) = else_stmt {
                        cg.lower_stmt(else_stmt)?;
                    }
                    cg.ensure_cir_yield(else_block);
                    Ok(())
                })?;

                self.builder.insert(
                    cir::ops::r#if(
                        self.context,
                        condition,
                        Some(then_region.id()),
                        Some(else_region.id()),
                    )
                    .build(),
                );
                Ok(())
            }
            AstKind::Break => {
                let scope = *self.loop_scopes.last().unwrap();
                self.builder
                    .insert(cir::ops::r#break(self.context, scope).build());
                self.terminated = true;
                Ok(())
            }
            AstKind::Continue => {
                let scope = *self.loop_scopes.last().unwrap();
                self.builder
                    .insert(cir::ops::r#continue(self.context, scope).build());
                self.terminated = true;
                Ok(())
            }
            kind => Err(unsupported(ast, stmt, format!("statement {kind:?}"))),
        }
    }

    fn in_block<T>(
        &mut self,
        block: std::sync::Arc<tir::Block>,
        lower: impl FnOnce(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        let outer = std::mem::replace(&mut self.builder, IRBuilder::new(block));
        let outer_terminated = std::mem::replace(&mut self.terminated, false);
        let result = lower(self);
        self.builder = outer;
        self.terminated = outer_terminated;
        result
    }

    fn ensure_cir_yield(&mut self, block: std::sync::Arc<tir::Block>) {
        let terminated = block.op_ids().last().is_some_and(|op| {
            self.context
                .get_op(*op)
                .as_interface::<dyn tir::Terminator>()
                .is_some()
        });
        if !terminated {
            self.builder.insert(cir::ops::r#yield(self.context).build());
        }
    }

    fn lower_condition(&mut self, expression: NodeId) -> Result<ValueId, Diagnostic> {
        let value = self.lower_expr(expression)?;
        let ty = self.context.get_value(value).ty();
        if ty == IntegerType::new(self.context, 1) {
            return Ok(value);
        }
        let zero = self
            .builder
            .insert(b::constant(self.context, 0, ty).build())
            .result();
        Ok(self
            .builder
            .insert(
                b::CmpIOpBuilder::new(self.context)
                    .lhs(value)
                    .rhs(zero)
                    .predicate("ne")
                    .result_type(IntegerType::new(self.context, 1))
                    .build(),
            )
            .result())
    }

    fn materialize(&mut self, expression: LoweredExpr) -> ValueId {
        match expression {
            LoweredExpr::Value(value) => value,
            LoweredExpr::Address { ptr, elem } => self
                .builder
                .insert(p::load(self.context, ptr, elem).build())
                .result(),
        }
    }

    fn lower_expr(&mut self, root: NodeId) -> Result<ValueId, Diagnostic> {
        let expression = self.lower_expr_value(root)?;
        Ok(self.materialize(expression))
    }

    fn lower_expr_value(&mut self, root: NodeId) -> Result<LoweredExpr, Diagnostic> {
        let ast = self.ast;
        self.values.clear();
        let mut base = root.index();

        for node in ast.postorder(root) {
            if self.values.is_empty() {
                base = node.index();
            }
            debug_assert_eq!(
                node.index(),
                base + self.values.len(),
                "subtree not contiguous"
            );

            let expression = match ast.get_node(node).kind {
                AstKind::Int => {
                    let AstLeaf::Int(n) = ast.get_leaf_data(node).unwrap() else {
                        unreachable!("int node carries an int payload");
                    };
                    let ty = lower_type(self.context, self.typed, node_type(self.typed, node));
                    LoweredExpr::Value(
                        self.builder
                            .insert(b::constant(self.context, n.value.to_i64(), ty).build())
                            .result(),
                    )
                }
                AstKind::SizeofType | AstKind::SizeofExpr => {
                    let value = ast.get_annotation(node).unwrap().constant.unwrap();
                    let ty = lower_type(self.context, self.typed, node_type(self.typed, node));
                    LoweredExpr::Value(
                        self.builder
                            .insert(b::constant(self.context, value, ty).build())
                            .result(),
                    )
                }
                AstKind::String => {
                    let AstLeaf::String(value) = ast.get_leaf_data(node).unwrap() else {
                        unreachable!("string node carries a string payload");
                    };
                    let i8_ty = IntegerType::new(self.context, 8);
                    let ptr_ty = PtrType::typed(self.context, i8_ty);
                    LoweredExpr::Value(
                        self.builder
                            .insert(cir::string_op(self.context, value, ptr_ty))
                            .result(),
                    )
                }
                AstKind::Var => {
                    let AstLeaf::Var(_) = ast.get_leaf_data(node).unwrap() else {
                        unreachable!("var node carries a var payload");
                    };
                    let slot = self.locals[&node_entity(self.typed, node)];
                    LoweredExpr::Address {
                        ptr: slot.ptr,
                        elem: slot.elem,
                    }
                }
                AstKind::Member => {
                    let AstLeaf::Member { indirect, .. } = ast.get_leaf_data(node).unwrap() else {
                        unreachable!("member node carries a member payload");
                    };
                    let base_node = ast.children(node).next().unwrap();
                    let base = self.values[base_node.index() - base];
                    let base_ptr = if *indirect {
                        self.materialize(base)
                    } else if let LoweredExpr::Address { ptr, .. } = base {
                        ptr
                    } else {
                        return Err(unsupported(
                            ast,
                            node,
                            "non-addressable member base".to_string(),
                        ));
                    };
                    let elem = lower_type(self.context, self.typed, node_type(self.typed, node));
                    let ptr_ty = if matches!(
                        self.typed.types().kind(node_type(self.typed, node)),
                        TypeKind::Record(_)
                    ) {
                        PtrType::opaque(self.context)
                    } else {
                        PtrType::typed(self.context, elem)
                    };
                    let field = ast.get_annotation(node).unwrap().member_index.unwrap() as u64;
                    let base_ty = node_type(self.typed, base_node);
                    let record = match self.typed.types().kind(base_ty) {
                        TypeKind::Record(id) => self.typed.record(*id).unwrap(),
                        TypeKind::Pointer(pointee) => {
                            let TypeKind::Record(id) = self.typed.types().kind(*pointee) else {
                                unreachable!("member base has a record type")
                            };
                            self.typed.record(*id).unwrap()
                        }
                        _ => unreachable!("member base has a record type"),
                    };
                    let member = self.builder.insert(
                        cir::ops::get_member(
                            self.context,
                            base_ptr,
                            field,
                            record.name.as_str(),
                            ptr_ty,
                        )
                        .build(),
                    );
                    LoweredExpr::Address {
                        ptr: member.result(),
                        elem,
                    }
                }
                AstKind::Call => {
                    let AstLeaf::Call(name) = ast.get_leaf_data(node).unwrap() else {
                        unreachable!("call node carries a call payload");
                    };
                    let sig = &self.signatures[&node_entity(self.typed, node)];
                    let arguments = ast
                        .children(node)
                        .map(|arg| self.values[arg.index() - base])
                        .collect::<Vec<_>>();
                    let args = arguments
                        .into_iter()
                        .map(|argument| self.materialize(argument))
                        .collect();
                    LoweredExpr::Value(
                        self.builder
                            .insert(b::call(self.context, args, name.as_str(), sig.ret).build())
                            .result(),
                    )
                }
                kind @ (AstKind::Add | AstKind::Sub | AstKind::Mul) => {
                    let mut children = ast.children(node);
                    let lhs = self.values[children.next().unwrap().index() - base];
                    let rhs = self.values[children.next().unwrap().index() - base];
                    let l = self.materialize(lhs);
                    let r = self.materialize(rhs);
                    let ty = lower_type(self.context, self.typed, node_type(self.typed, node));
                    let value = match kind {
                        AstKind::Add => self
                            .builder
                            .insert(b::addi(self.context, l, r, ty).build())
                            .result(),
                        AstKind::Sub => self
                            .builder
                            .insert(b::subi(self.context, l, r, ty).build())
                            .result(),
                        _ => self
                            .builder
                            .insert(b::muli(self.context, l, r, ty).build())
                            .result(),
                    };
                    LoweredExpr::Value(value)
                }
                kind @ (AstKind::BitAnd
                | AstKind::BitXor
                | AstKind::BitOr
                | AstKind::Shl
                | AstKind::Shr) => {
                    let mut children = ast.children(node);
                    let lhs = self.values[children.next().unwrap().index() - base];
                    let rhs = self.values[children.next().unwrap().index() - base];
                    let lhs = self.materialize(lhs);
                    let rhs = self.materialize(rhs);
                    let source_ty = node_type(self.typed, node);
                    let ty = lower_type(self.context, self.typed, source_ty);
                    let value = match kind {
                        AstKind::BitAnd => self
                            .builder
                            .insert(b::andi(self.context, lhs, rhs, ty).build())
                            .result(),
                        AstKind::BitXor => self
                            .builder
                            .insert(b::xori(self.context, lhs, rhs, ty).build())
                            .result(),
                        AstKind::BitOr => self
                            .builder
                            .insert(b::ori(self.context, lhs, rhs, ty).build())
                            .result(),
                        AstKind::Shl => self
                            .builder
                            .insert(b::shli(self.context, lhs, rhs, ty).build())
                            .result(),
                        AstKind::Shr if self.typed.integer_is_signed(source_ty).unwrap() => self
                            .builder
                            .insert(b::shrsi(self.context, lhs, rhs, ty).build())
                            .result(),
                        AstKind::Shr => self
                            .builder
                            .insert(b::shrui(self.context, lhs, rhs, ty).build())
                            .result(),
                        _ => unreachable!(),
                    };
                    LoweredExpr::Value(value)
                }
                kind @ (AstKind::Neg | AstKind::Pos | AstKind::Not | AstKind::BitNot) => {
                    let child = ast.children(node).next().unwrap();
                    let operand = self.materialize(self.values[child.index() - base]);
                    let result_ty =
                        lower_type(self.context, self.typed, node_type(self.typed, node));
                    let value = match kind {
                        AstKind::Pos => operand,
                        AstKind::Neg => {
                            let zero = self
                                .builder
                                .insert(b::constant(self.context, 0, result_ty).build())
                                .result();
                            self.builder
                                .insert(b::subi(self.context, zero, operand, result_ty).build())
                                .result()
                        }
                        AstKind::BitNot => {
                            let ones = self
                                .builder
                                .insert(b::constant(self.context, -1, result_ty).build())
                                .result();
                            self.builder
                                .insert(b::xori(self.context, operand, ones, result_ty).build())
                                .result()
                        }
                        AstKind::Not => {
                            let operand_ty =
                                lower_type(self.context, self.typed, node_type(self.typed, child));
                            let zero = self
                                .builder
                                .insert(b::constant(self.context, 0, operand_ty).build())
                                .result();
                            let comparison = self
                                .builder
                                .insert(
                                    b::CmpIOpBuilder::new(self.context)
                                        .lhs(operand)
                                        .rhs(zero)
                                        .predicate("eq")
                                        .result_type(IntegerType::new(self.context, 1))
                                        .build(),
                                )
                                .result();
                            self.builder
                                .insert(b::extui(self.context, comparison, result_ty).build())
                                .result()
                        }
                        _ => unreachable!(),
                    };
                    LoweredExpr::Value(value)
                }
                kind @ (AstKind::Lt
                | AstKind::Gt
                | AstKind::Le
                | AstKind::Ge
                | AstKind::Eq
                | AstKind::Ne) => {
                    let mut children = ast.children(node);
                    let lhs_node = children.next().unwrap();
                    let rhs_node = children.next().unwrap();
                    let lhs = self.materialize(self.values[lhs_node.index() - base]);
                    let rhs = self.materialize(self.values[rhs_node.index() - base]);
                    let signed = self
                        .typed
                        .integer_is_signed(node_type(self.typed, lhs_node))
                        .unwrap_or(true);
                    let predicate = match (kind, signed) {
                        (AstKind::Lt, true) => "slt",
                        (AstKind::Lt, false) => "ult",
                        (AstKind::Gt, true) => "sgt",
                        (AstKind::Gt, false) => "ugt",
                        (AstKind::Le, true) => "sle",
                        (AstKind::Le, false) => "ule",
                        (AstKind::Ge, true) => "sge",
                        (AstKind::Ge, false) => "uge",
                        (AstKind::Eq, _) => "eq",
                        (AstKind::Ne, _) => "ne",
                        _ => unreachable!(),
                    };
                    LoweredExpr::Value(
                        self.builder
                            .insert(
                                b::CmpIOpBuilder::new(self.context)
                                    .lhs(lhs)
                                    .rhs(rhs)
                                    .predicate(predicate)
                                    .result_type(IntegerType::new(self.context, 1))
                                    .build(),
                            )
                            .result(),
                    )
                }
                AstKind::Comma => {
                    let rhs = ast.children(node).nth(1).unwrap();
                    LoweredExpr::Value(self.materialize(self.values[rhs.index() - base]))
                }
                AstKind::AssignExpr => {
                    let mut children = ast.children(node);
                    let lhs_node = children.next().unwrap();
                    let lhs = self.values[lhs_node.index() - base];
                    let rhs = self.values[children.next().unwrap().index() - base];
                    let LoweredExpr::Address { ptr, elem } = lhs else {
                        return Err(unsupported(
                            ast,
                            node,
                            "non-addressable assignment".to_string(),
                        ));
                    };
                    if let TypeKind::Record(id) =
                        self.typed.types().kind(node_type(self.typed, lhs_node))
                    {
                        let LoweredExpr::Address { ptr: source, .. } = rhs else {
                            return Err(unsupported(
                                ast,
                                node,
                                "non-addressable struct source".to_string(),
                            ));
                        };
                        self.builder.insert(
                            cir::ops::copy_struct(
                                self.context,
                                ptr,
                                source,
                                self.typed.record(*id).unwrap().name.as_str(),
                            )
                            .build(),
                        );
                        LoweredExpr::Address { ptr, elem }
                    } else {
                        let value = self.materialize(rhs);
                        self.builder
                            .insert(p::store(self.context, value, ptr).build());
                        LoweredExpr::Value(value)
                    }
                }
                // The richer operators (division, comparison, logical, unary,
                // calls) are parsed but not yet lowered; stub them out for now.
                kind => {
                    return Err(unsupported(ast, node, format!("expression {kind:?}")));
                }
            };
            let expression = if ast
                .get_annotation(node)
                .is_some_and(|semantics| !semantics.conversions.is_empty())
            {
                let value = self.materialize(expression);
                LoweredExpr::Value(self.apply_conversions(node, value))
            } else {
                expression
            };
            self.values.push(expression);
        }

        Ok(*self.values.last().unwrap())
    }
}

/// Decode the C escape sequences of a string literal's source text into the
/// bytes the program observes. `cir.string` keeps the source spelling; the
/// hoisted data must hold real bytes.
fn decode_c_escapes(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Hoist every `cir.string` into a module-level `.rodata` section and rewrite
/// its use into a `builtin.addr_of` of the string's local symbol; identical
/// literals share one symbol. Runs only ahead of the machine backend — the
/// asm-dialect data ops it creates mean nothing to earlier stages.
pub fn hoist_strings(context: &Context, module: &ModuleOp) -> Result<(), tir::PassError> {
    use tir::attributes::AttributeValue;
    use tir::backend::{
        LiteralOpBuilder, SectionEndOpBuilder, SectionOpBuilder, SymbolEndOpBuilder,
        SymbolOpBuilder,
    };

    let mut rewriter = tir::Rewriter::new(context.clone());
    let mut strings: Vec<(String, String)> = Vec::new();
    let mut labels: HashMap<String, String> = HashMap::new();

    for op_id in module.body().op_ids() {
        let op = context.get_op(op_id);
        if op.clone().as_op::<tir::builtin::FuncOp>().is_none() {
            continue;
        }
        let region = context.get_region(op.regions[0]);
        for block in region.iter(context.clone()) {
            for op_id in block.op_ids() {
                let op = context.get_op(op_id);
                let Some(string) = op.clone().as_op::<cir::StringOp>() else {
                    continue;
                };
                let value = string
                    .attributes()
                    .iter()
                    .find(|attr| attr.name == "value")
                    .and_then(|attr| match &attr.value {
                        AttributeValue::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .expect("cir.string must carry a value");
                let label = labels
                    .entry(value.clone())
                    .or_insert_with(|| {
                        let label = format!(".L.str{}", strings.len());
                        strings.push((label.clone(), decode_c_escapes(&value)));
                        label
                    })
                    .clone();
                let result_ty = context.get_value(string.result()).ty();
                let addr = b::addr_of_op(context, &label, result_ty);
                rewriter.replace_op(
                    &tir::OperationRef::new(op, Some(block.clone()), None),
                    &addr,
                )?;
            }
        }
    }

    if strings.is_empty() {
        return Ok(());
    }

    let section = SectionOpBuilder::new(context)
        .attr("name", AttributeValue::Str(".rodata".to_string()))
        .build();
    let mut section_builder = IRBuilder::new(section.body());
    for (label, value) in strings {
        let symbol = SymbolOpBuilder::new(context)
            .attr("name", AttributeValue::Str(label))
            .attr("binding", AttributeValue::Str("local".to_string()))
            .attr("kind", AttributeValue::Str("object".to_string()))
            .build();
        let mut symbol_builder = IRBuilder::new(symbol.body());
        symbol_builder.insert(
            LiteralOpBuilder::new(context)
                .attr("kind", AttributeValue::Str("asciz".to_string()))
                .attr("value", AttributeValue::Str(value))
                .build(),
        );
        symbol_builder.insert(SymbolEndOpBuilder::new(context).build());
        section_builder.insert(symbol);
    }
    section_builder.insert(SectionEndOpBuilder::new(context).build());

    // Splice the section in ahead of the module terminator.
    let body = module.body();
    let end = body.op_ids().len().saturating_sub(1);
    body.insert(end, section.id());
    Ok(())
}
