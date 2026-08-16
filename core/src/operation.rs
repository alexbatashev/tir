use crate::{
    BlockId, Context, ContextIterator, Error, GetFromContext,
    context::ContextRef,
    ir_formatter::IRFormatter,
    parse::{Span, text::Parser as IRParser},
    region::RegionId,
    value::ValueId,
};
use std::{any::Any, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationName(&'static str);

impl OperationName {
    pub fn of<T: Operation>() -> Self {
        Self(T::name())
    }

    /// Returns the textual spelling used by IR parsers and printers.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for OperationName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::fmt::Debug for OperationName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OperationName").field(&self.0).finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DialectName(&'static str);

impl DialectName {
    pub fn of<T: crate::Dialect>() -> Self {
        Self(T::name())
    }

    pub fn of_operation<T: Operation>() -> Self {
        Self(T::dialect())
    }

    /// Returns the textual spelling used by IR parsers and printers.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for DialectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::fmt::Debug for DialectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DialectName").field(&self.0).finish()
    }
}

pub type ErasedOpInterface = Box<dyn Any>;
pub type OpInterfaceConverter = fn(Arc<OpInstance>) -> ErasedOpInterface;

struct InterfaceValue<I: ?Sized + 'static>(Box<I>);

pub trait ImplementsOpInterface<I: ?Sized + 'static>: Operation {
    fn into_interface(self: Box<Self>) -> Box<I>;
}

pub fn erase_op_interface<I: ?Sized + 'static>(value: Box<I>) -> ErasedOpInterface {
    Box::new(InterfaceValue::<I>(value))
}

pub fn downcast_op_interface<I: ?Sized + 'static>(erased: ErasedOpInterface) -> Option<Box<I>> {
    erased
        .downcast::<InterfaceValue<I>>()
        .ok()
        .map(|boxed| boxed.0)
}

pub fn op_interface_converter<Op, I>(instance: Arc<OpInstance>) -> ErasedOpInterface
where
    Op: ImplementsOpInterface<I>,
    I: ?Sized + 'static,
{
    let op = Box::new(Op::from_op_instance(instance));
    erase_op_interface(ImplementsOpInterface::<I>::into_interface(op))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(u32);

/// Core trait for all operations in TIR’s intermediate representation.
///
/// An `Operation` is the fundamental building block of the IR graph,
/// forming nodes that represent everything from high-level constructs to low-level code.
/// Each operation models a transformation or computation in the program,
/// and is used to describe program modules, functions, control flow, arithmetic, memory access,
/// and target-specific instructions.
///
/// All language constructs, from an entire module to a single arithmetic instruction,
/// are expressed as operations. This unified abstraction allows for powerful analyses,
/// transformations, and extension with new operation kinds through custom dialects.
///
/// # Example
///
/// Defining and using a custom operation:
/// ```rust
/// use tir_macros::operation;
///
/// operation! {
///     BarOp {
///         name: "bar",
///         dialect: "foo",
///     }
/// }
/// ```
///
/// This macro will generate a BarOp structure, as well as BarOpBuilder for constructing
/// custom operation.
///
/// Because all operations implement this trait, generic IR passes can inspect,
/// transform, or analyze any construct in the IR using the same programming model.
pub trait Operation: 'static + Send + Sync + Any + Verifiable + OpDefVerifiable {
    fn name() -> &'static str
    where
        Self: Sized;
    fn dialect() -> &'static str
    where
        Self: Sized;

    fn id(&self) -> OpId {
        self.op_instance().id
    }

    /// The erased instance backing this op. The `operation!` macro wires this to
    /// the newtype field; most trait methods delegate to it by default so
    /// generated ops carry no per-op copies of the plumbing.
    #[doc(hidden)]
    fn op_instance(&self) -> &Arc<OpInstance>;

    fn from_op_instance(instance: Arc<OpInstance>) -> Self
    where
        Self: Sized;

    fn from_op_instance_dyn(instance: Arc<OpInstance>) -> Box<dyn Operation>
    where
        Self: Sized;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    fn print<'a, 'b: 'a>(&'a self, fmt: &'a mut IRFormatter<'b>) -> Result<(), std::fmt::Error>;
    fn parse<'src>(
        parser: &mut IRParser<'src>,
        context: &Context,
    ) -> Result<Box<dyn Operation>, (Span, Error)>
    where
        Self: Sized;

    fn regions(&self) -> ContextIterator<RegionId> {
        let instance = self.op_instance();
        let context = instance.context.upgrade();
        ContextIterator::new(context, instance.regions.clone())
    }

    fn operands(&self) -> &[ValueId] {
        &self.op_instance().operands
    }

    fn attributes(&self) -> &[crate::attributes::NamedAttribute] {
        &self.op_instance().attributes
    }

    /// The value of the attribute called `name`; see [`OpInstance::attr`].
    fn attr(&self, name: &str) -> Option<&crate::attributes::AttributeValue> {
        self.op_instance().attr(name)
    }

    fn operand_names(&self) -> &'static [&'static str] {
        &[]
    }

    fn semantic_expr(&self, _g: &mut crate::sem::SemGraph) -> Option<crate::graph::NodeId> {
        None
    }

    fn register_interfaces(_context: &Context)
    where
        Self: Sized,
    {
    }

    fn parent_block(&self) -> Option<BlockId> {
        self.op_instance().parent_block()
    }

    /// Verifies that operation is valid.
    ///
    /// Order of verification:
    /// 1. Operands verification - all operands must exist and value types must match.
    /// 2. Attributes verification - all DSL-defined attributes must have a value and types must match.
    /// 3. Interface verification - same operand types, etc...
    /// 4. Region verification - all basic blocks must end with a terminator, there must be at least one basic block.
    /// 5. Custom verification - additional constraints imposed by dialect authors.
    fn verify(&self, context: &Context) -> Result<(), crate::Error> {
        self.verify_operands(context)?;
        self.verify_attributes(context)?;
        self.verify_interfaces(context)?;

        for r in self.regions() {
            r.verify(context)?;
        }

        self.verify_impl(context)?;

        Ok(())
    }
}

pub fn verify_op_tree(context: &Context, op_id: OpId) -> Result<(), Error> {
    verify_op_tree_ops(context, op_id)?;
    verify_state_linearity(context, op_id)
}

/// Checks that memory state stays a linear chain: a `!state` value names the memory
/// at one point in the program, so consuming it twice would describe two futures for
/// the same memory. Loop-carried state crosses a region boundary as a carried
/// argument, which is a fresh value, so a single walk of the whole tree suffices.
fn verify_state_linearity(context: &Context, op_id: OpId) -> Result<(), Error> {
    let state = crate::builtin::StateType::new(context);
    let mut consumed = std::collections::HashSet::new();
    let mut worklist = vec![op_id];
    while let Some(op_id) = worklist.pop() {
        let instance = context.get_op(op_id);
        for operand in &instance.operands {
            if !context.has_value(*operand) || context.get_value(*operand).ty() != state {
                continue;
            }
            if !consumed.insert(*operand) {
                return Err(Error::VerificationError(format!(
                    "state value %{} is used more than once",
                    operand.number()
                )));
            }
        }
        for region_id in instance.regions.iter().rev() {
            for block in context.get_region(*region_id).iter(context.clone()) {
                worklist.extend(block.op_ids().iter().rev());
            }
        }
    }
    Ok(())
}

/// Checks that an op wires memory state through ports read off its end: the
/// `!state` values it takes are its trailing ones.
///
/// An op that accesses memory has one, since it observes one memory, and its own
/// arity rules say so. A gate or a loop accesses nothing and instead carries the
/// chains that cross it, one port each, so the count is what crosses rather than
/// one.
fn verify_state_ports(context: &Context, instance: &Arc<OpInstance>) -> Result<(), Error> {
    let state = crate::builtin::StateType::new(context);
    let ports = |values: &[crate::ValueId], kind: &str| {
        let is_state =
            |id: &crate::ValueId| context.has_value(*id) && context.get_value(*id).ty() == state;
        let Some(first) = values.iter().position(is_state) else {
            return Ok(());
        };
        if !values[first..].iter().all(is_state) {
            return Err(Error::VerificationError(format!(
                "an operation's !state {kind}s must be its trailing ones"
            )));
        }
        Ok(())
    };
    ports(&instance.operands, "operand")?;
    ports(&instance.results, "result")
}

fn verify_op_tree_ops(context: &Context, op_id: OpId) -> Result<(), Error> {
    if !context.has_operation(op_id) {
        return Err(Error::VerificationError(format!(
            "operation {op_id:?} does not exist"
        )));
    }

    let instance = context.get_op(op_id);
    verify_token_region_arguments(context, &instance)?;
    verify_scoped_metadata(&instance)?;
    verify_state_ports(context, &instance)?;
    instance.clone().as_dyn_op().verify(context)?;

    for region_id in instance.regions.clone() {
        let region = context.get_region(region_id);
        for block in region.iter(context.clone()) {
            for child in block.op_ids() {
                verify_op_tree_ops(context, child)?;
            }
        }
    }

    Ok(())
}

/// Checks the metadata specs this op contributes to its nested scopes.
fn verify_scoped_metadata(instance: &Arc<OpInstance>) -> Result<(), Error> {
    if let Some(value) = instance.attr(crate::DATA_LAYOUT) {
        crate::layout::verify_spec(value)?;
    }
    if let Some(value) = instance.attr(crate::TARGET_ENV) {
        crate::target_env::verify_spec(value)?;
    }
    Ok(())
}

fn verify_token_region_arguments(
    context: &Context,
    instance: &Arc<OpInstance>,
) -> Result<(), Error> {
    let token = crate::builtin::TokenType::new(context);
    let scope_regions = instance
        .clone()
        .as_interface::<dyn crate::TokenScope>()
        .map(|scope| scope.token_scope_regions())
        .unwrap_or_default();
    for (region_index, region_id) in instance.regions.iter().enumerate() {
        for (block_index, block) in context
            .get_region(*region_id)
            .iter(context.clone())
            .enumerate()
        {
            for argument in block.arguments() {
                if argument.ty() != token {
                    continue;
                }
                if block_index != 0 || !scope_regions.contains(&instance.regions[region_index]) {
                    return Err(Error::VerificationError(
                        "token values are only allowed as loop body entry arguments".to_string(),
                    ));
                }
            }
        }
    }
    if instance.is::<crate::builtin::FuncOp>()
        && matches!(
            instance.attr("ret_type"),
            Some(crate::attributes::AttributeValue::Type(ty)) if *ty == token
        )
    {
        return Err(Error::VerificationError(
            "token values are not allowed in function signatures".to_string(),
        ));
    }
    Ok(())
}

pub trait Verifiable {
    fn verify_impl(&self, _context: &Context) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Common functions for verifying basic operation properties, that can be inferred
/// from DSL operation definition. Users are discouraged from implemnting this trait
/// manually. Prefer auto-generated DSL implementation instead.
pub trait OpDefVerifiable {
    /// Verify operands are correct. That is, values exist in the context and have the
    /// same type as described in DSL.
    fn verify_operands(&self, context: &Context) -> Result<(), crate::Error>;
    /// Verify attributes are correct. For each attribute that is defined in the DSL
    /// a value must exist and its types must match.
    fn verify_attributes(&self, context: &Context) -> Result<(), crate::Error>;
    /// Verify interfaces. For each implemented interface a verify_interface
    /// function is called.
    fn verify_interfaces(&self, _context: &Context) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Static operand/result specification for [`OpDefVerifiable`], emitted per op by
/// the `operation!` macro. The verification engine lives in core so a generated
/// op carries only this table instead of an inlined copy of the engine.
pub struct OpDefSpec {
    pub operands: &'static [(&'static str, &'static str)],
    pub results: &'static [(&'static str, &'static str)],
    pub operand_constraint_names: &'static [&'static str],
    pub result_constraint_names: &'static [&'static str],
    pub operand_constraint_checkers: &'static [fn(&dyn crate::Type) -> bool],
    pub result_constraint_checkers: &'static [fn(&dyn crate::Type) -> bool],
    pub variadic_operands: bool,
    pub variadic_result: bool,
    pub optional_result: bool,
    pub state_output: bool,
}

fn verify_def_value(
    context: &Context,
    op_name: &str,
    kind: &str,
    value_name: &str,
    value_id: ValueId,
    checker: fn(&dyn crate::Type) -> bool,
    constraint_name: &str,
) -> Result<(), crate::Error> {
    if !context.has_value(value_id) {
        return Err(crate::Error::VerificationError(format!(
            "{op_name} {kind} '{value_name}' references unknown value %{id}",
            id = value_id.number()
        )));
    }

    let value = context.get_value(value_id);
    match value.defining_op() {
        Some(def_op) => {
            if !context.has_operation(def_op) {
                return Err(crate::Error::VerificationError(format!(
                    "{op_name} {kind} '{value_name}' value %{id} references missing defining op",
                    id = value_id.number()
                )));
            }
        }
        None => {
            let detail = if kind == "operand" {
                "has no defining op and is not a block argument"
            } else {
                "has no defining op"
            };
            if kind != "operand" || !context.is_block_argument(value_id) {
                return Err(crate::Error::VerificationError(format!(
                    "{op_name} {kind} '{value_name}' value %{id} {detail}",
                    id = value_id.number()
                )));
            }
        }
    }

    let actual_ty = value.ty();
    let actual_ty_data = context.get_type_data(actual_ty);
    if !checker(actual_ty_data.as_ref()) {
        return Err(crate::Error::VerificationError(format!(
            "{op_name} {kind} '{value_name}' expected constraint {constraint_name}, got {}",
            context.type_to_string(actual_ty)
        )));
    }
    Ok(())
}

/// [`OpDefVerifiable::verify_operands`] engine, driven by an [`OpDefSpec`].
pub fn verify_opdef_operands(
    context: &Context,
    instance: &OpInstance,
    op_name: &str,
    spec: &OpDefSpec,
) -> Result<(), crate::Error> {
    let operands = &instance.operands;

    if spec.variadic_operands {
        // Variadic ops recover their operand grouping from the segment sizes
        // recorded at build time, then validate each declared operand's segment.
        let segment_sizes: Vec<usize> = match instance.attr("operand_segment_sizes") {
            Some(crate::attributes::AttributeValue::Array(items)) => items
                .iter()
                .map(|v| match v {
                    crate::attributes::AttributeValue::UInt(n) => *n as usize,
                    _ => 0usize,
                })
                .collect(),
            _ => {
                return Err(crate::Error::VerificationError(format!(
                    "{op_name} missing operand_segment_sizes attribute"
                )));
            }
        };

        if segment_sizes.len() != spec.operands.len() {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} expects {} operand segments, got {}",
                spec.operands.len(),
                segment_sizes.len()
            )));
        }

        let total: usize = segment_sizes.iter().sum();
        if total != operands.len() {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} operand segment sizes sum to {total}, but it has {} operands",
                operands.len()
            )));
        }

        let mut cursor = 0usize;
        for (idx, (operand_name, _)) in spec.operands.iter().enumerate() {
            for k in 0..segment_sizes[idx] {
                verify_def_value(
                    context,
                    op_name,
                    "operand",
                    operand_name,
                    operands[cursor + k],
                    spec.operand_constraint_checkers[idx],
                    spec.operand_constraint_names[idx],
                )?;
            }
            cursor += segment_sizes[idx];
        }
    } else {
        if operands.len() > spec.operands.len() {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} expects at most {} operands, got {}",
                spec.operands.len(),
                operands.len()
            )));
        }

        for (idx, (operand_name, type_spec)) in spec.operands.iter().enumerate() {
            let is_optional = type_spec.starts_with('?');

            let Some(value_id) = operands.get(idx).copied() else {
                if is_optional {
                    continue;
                }
                return Err(crate::Error::VerificationError(format!(
                    "{op_name} missing required operand '{operand_name}'"
                )));
            };

            verify_def_value(
                context,
                op_name,
                "operand",
                operand_name,
                value_id,
                spec.operand_constraint_checkers[idx],
                spec.operand_constraint_names[idx],
            )?;
        }
    }

    // The number of results that carry a value, i.e. all but a trailing state port.
    let value_results_len = if spec.state_output {
        let state = crate::builtin::StateType::new(context);
        let has_state = instance
            .results
            .last()
            .is_some_and(|id| context.has_value(*id) && context.get_value(*id).ty() == state);
        instance.results.len() - has_state as usize
    } else {
        instance.results.len()
    };

    if spec.variadic_result {
        // A variadic result declares one spec covering every result value.
    } else if spec.optional_result {
        if value_results_len > spec.results.len() {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} expects at most {} results, got {}",
                spec.results.len(),
                value_results_len
            )));
        }
    } else if value_results_len != spec.results.len() {
        return Err(crate::Error::VerificationError(format!(
            "{op_name} expects {} results, got {}",
            spec.results.len(),
            value_results_len
        )));
    }

    for result_index in 0..value_results_len {
        let idx = if spec.variadic_result {
            0
        } else {
            result_index
        };
        let (result_name, _) = spec.results[idx];
        verify_def_value(
            context,
            op_name,
            "result",
            result_name,
            instance.results[result_index],
            spec.result_constraint_checkers[idx],
            spec.result_constraint_names[idx],
        )?;
    }

    Ok(())
}

fn attr_type_matches(attr_type: &str, value: &crate::attributes::AttributeValue) -> bool {
    use crate::attributes::AttributeValue as V;
    match attr_type {
        "any" => true,
        "Str" => matches!(value, V::Str(_)),
        "Int" => matches!(value, V::Int(_)),
        "UInt" => matches!(value, V::UInt(_)),
        "F32" => matches!(value, V::F32(_)),
        "F64" => matches!(value, V::F64(_)),
        "Bool" => matches!(value, V::Bool(_)),
        "Array" => matches!(value, V::Array(_)),
        "Dict" => matches!(value, V::Dict(_)),
        "Register" => matches!(value, V::Register(_)),
        "Type" => matches!(value, V::Type(_)),
        "Block" => matches!(value, V::Block(_)),
        _ => false,
    }
}

/// [`OpDefVerifiable::verify_attributes`] engine, driven by `(name, type)` specs.
pub fn verify_opdef_attributes(
    context: &Context,
    instance: &OpInstance,
    op_name: &str,
    attr_specs: &[(&str, &str)],
) -> Result<(), crate::Error> {
    let _ = context;
    for (attr_name, attr_type) in attr_specs {
        let Some(value) = instance.attr(attr_name) else {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} missing required attribute '{attr_name}'"
            )));
        };

        if !attr_type_matches(attr_type, value) {
            return Err(crate::Error::VerificationError(format!(
                "{op_name} attribute '{attr_name}' expected type '{attr_type}'"
            )));
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct OpInstance {
    pub id: OpId,
    name: &'static str,
    dialect: &'static str,
    pub context: ContextRef,
    pub operands: Vec<ValueId>,
    pub results: Vec<ValueId>,
    pub regions: Vec<RegionId>,
    pub attributes: Vec<crate::attributes::NamedAttribute>,
}

impl OpInstance {
    pub fn new<T: Operation>(
        context: ContextRef,
        operands: Vec<ValueId>,
        results: Vec<ValueId>,
        regions: Vec<RegionId>,
        attributes: Vec<crate::attributes::NamedAttribute>,
    ) -> Self {
        Self {
            id: OpId::invalid(),
            name: T::name(),
            dialect: T::dialect(),
            context,
            operands,
            results,
            regions,
            attributes,
        }
    }

    /// Constructs an operation selected from textual input at a parser boundary.
    pub fn new_dynamic(
        identity: (&'static str, &'static str),
        context: ContextRef,
        operands: Vec<ValueId>,
        results: Vec<ValueId>,
        regions: Vec<RegionId>,
        attributes: Vec<crate::attributes::NamedAttribute>,
    ) -> Self {
        let (dialect, name) = identity;
        Self {
            id: OpId::invalid(),
            name,
            dialect,
            context,
            operands,
            results,
            regions,
            attributes,
        }
    }

    /// Returns an opaque name for textual output, not operation identity.
    ///
    /// ```compile_fail
    /// fn is_function(op: &tir::OpInstance) -> bool {
    ///     op.name() == "func"
    /// }
    /// ```
    pub fn name(&self) -> OperationName {
        OperationName(self.name)
    }

    pub fn dialect(&self) -> DialectName {
        DialectName(self.dialect)
    }

    /// The value of the attribute called `name`, resolving the name through the
    /// owning context's interner.
    pub fn attr(&self, name: &str) -> Option<&crate::attributes::AttributeValue> {
        let sym = self.context.upgrade().sym(name)?;
        self.attr_sym(sym)
    }

    /// [`OpInstance::attr`] for a name already interned, which is the form a
    /// repeated lookup wants: the comparison is on `u32`s.
    pub fn attr_sym(&self, name: tir_adt::Sym) -> Option<&crate::attributes::AttributeValue> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .map(|attribute| &attribute.value)
    }

    /// The block that holds this operation, or `None` if it is detached or the root.
    pub fn parent_block(&self) -> Option<crate::BlockId> {
        self.context.upgrade().parent_block(self.id)
    }

    /// Returns whether this instance has operation type `T`.
    pub fn is<T: Operation>(&self) -> bool {
        self.dialect == T::dialect() && self.name == T::name()
    }

    pub(crate) fn is_name(&self, dialect: &str, name: &str) -> bool {
        self.dialect == dialect && self.name == name
    }

    pub fn as_op<T: Operation + Sized>(self: Arc<Self>) -> Option<T> {
        if self.is::<T>() {
            Some(T::from_op_instance(self))
        } else {
            None
        }
    }

    pub fn as_dyn_op(self: Arc<Self>) -> Box<dyn Operation> {
        let context = self.context.upgrade();
        context.get_dyn_op(self.clone())
    }

    pub fn as_interface<I: ?Sized + 'static>(self: Arc<Self>) -> Option<Box<I>> {
        let context = self.context.upgrade();
        context.get_op_interface::<I>(self.clone())
    }

    pub fn has_interface<I: ?Sized + 'static>(&self) -> bool {
        self.context
            .upgrade()
            .find_op_interface::<I>(self)
            .is_some()
    }
}

impl Default for OpId {
    fn default() -> Self {
        Self(u32::MAX)
    }
}

impl OpId {
    pub fn invalid() -> Self {
        Self::default()
    }

    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }

    /// Raw integer id, for stable identification across an FFI boundary.
    pub fn number(self) -> u32 {
        self.0
    }

    /// Reconstruct an id from its raw integer, the inverse of [`OpId::number`].
    pub fn from_number(id: u32) -> Self {
        Self(id)
    }
}

impl GetFromContext for OpId {
    type Item = Arc<OpInstance>;

    fn get_from_context(&self, context: &crate::Context) -> Self::Item {
        context.get_op(*self)
    }
}
