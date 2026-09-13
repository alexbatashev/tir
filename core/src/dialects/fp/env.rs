use tir_adt::{APInt, RoundingMode};

use super::resource::{effect_records, state_for, transition_semantics, verify_ports};
use super::{EnvironmentType, RoundingType, resource};
use crate::builtin::StateResource;
use crate::graph::{MetaMutDag, NodeId};
use crate::sem::{SemGraph, SymKind};
use crate::{
    Context, Error, HasResourceSemantics, ResourceAccess, ResourceEffect, ResourceEffects,
    ResourceField, ResourceSemantics, ValueId, operation,
};

use crate as tir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatEnvironment {
    pub rounding: RoundingMode,
    pub flags: u8,
    pub traps: u8,
}

impl Default for FloatEnvironment {
    fn default() -> Self {
        Self {
            rounding: RoundingMode::TiesToEven,
            flags: 0,
            traps: 0,
        }
    }
}

operation! {
    RoundingConstantOp {
        name: "rounding_constant", dialect: "fp",
        attributes: A { mode: "Str" },
        results: R { result: "RoundingType" },
        interfaces: [crate::interp::Interp, crate::Speculatable],
        verifier: "true",
    }
}

macro_rules! env_op {
    (
        $op:ident, $name:tt, $access:expr,
        $ports:ident: $group:ident { $port:ident: $ty:tt },
        evaluate($operands:ident, $state:ident) $evaluate:block
    ) => {
        operation! {
            $op {
                name: $name, dialect: "fp",
                $ports: $group { $port: $ty },
                interfaces: [ResourceEffects, HasResourceSemantics, crate::interp::Interp],
                state: "in_out", verifier: "true",
            }
        }
        impl tir::Verifiable for $op {
            fn verify_impl(&self, _context: &Context) -> Result<(), Error> {
                verify_ports(&self.0, &required_effects($access, false))
            }
        }
        impl ResourceEffects for $op {
            fn resource_effects(&self) -> Vec<ResourceEffect> {
                effect_records(&self.0, required_effects($access, false))
            }
        }
        impl crate::interp::Interp for $op {
            fn evaluate(
                &self,
                $operands: &[crate::interp::Value],
                $state: &mut crate::interp::ExecutionState,
            ) -> Result<Vec<crate::interp::Value>, crate::interp::InterpError> $evaluate
        }
    };
}

env_op!(GetRoundOp, "get_round", ResourceAccess::Read,
    results: R { result: "RoundingType" },
    evaluate(_operands, state) {
        Ok(vec![crate::interp::Value::Rounding(state.fp_environment.rounding)])
    }
);
env_op!(SetRoundOp, "set_round", ResourceAccess::Change,
    operands: O { mode: "RoundingType" },
    evaluate(operands, state) {
        let [crate::interp::Value::Rounding(mode)] = operands else {
            return Err(crate::interp::InterpError::Message(
                "fp.set_round requires !fp.rounding".into(),
            ));
        };
        state.fp_environment.rounding = *mode;
        Ok(Vec::new())
    }
);
env_op!(GetFlagsOp, "get_flags", ResourceAccess::Read,
    results: R { flags: "crate::Integer<5>" },
    evaluate(_operands, state) {
        Ok(vec![crate::interp::Value::Int(APInt::new(
            5,
            state.fp_environment.flags as u64,
        ))])
    }
);
env_op!(ClearFlagsOp, "clear_flags", ResourceAccess::Change,
    operands: O { flags: "crate::Integer<5>" },
    evaluate(operands, state) {
        state.fp_environment.flags &= !mask(&operands[0])?;
        Ok(Vec::new())
    }
);

operation! {
    RaiseFlagsOp {
        name: "raise_flags", dialect: "fp",
        operands: O { flags: "crate::Integer<5>" },
        attributes: A { trapping: "Bool" },
        interfaces: [ResourceEffects, HasResourceSemantics, crate::interp::Interp],
        state: "in_out", verifier: "true",
    }
}

env_op!(GetTrapsOp, "get_traps", ResourceAccess::Read,
    results: R { traps: "crate::Integer<5>" },
    evaluate(_operands, state) {
        Ok(vec![crate::interp::Value::Int(APInt::new(
            5,
            state.fp_environment.traps as u64,
        ))])
    }
);
env_op!(SetTrapsOp, "set_traps", ResourceAccess::Change,
    operands: O { traps: "crate::Integer<5>" },
    evaluate(operands, state) {
        state.fp_environment.traps = mask(&operands[0])?;
        Ok(Vec::new())
    }
);
env_op!(SaveOp, "save", ResourceAccess::Read,
    results: R { snapshot: "EnvironmentType" },
    evaluate(_operands, state) {
        Ok(vec![crate::interp::Value::FpEnvironment(state.fp_environment)])
    }
);
env_op!(RestoreOp, "restore", ResourceAccess::Change,
    operands: O { snapshot: "EnvironmentType" },
    evaluate(operands, state) {
        let [crate::interp::Value::FpEnvironment(snapshot)] = operands else {
            return Err(crate::interp::InterpError::Message(
                "fp.restore requires !fp.env".into(),
            ));
        };
        state.fp_environment = *snapshot;
        Ok(Vec::new())
    }
);
env_op!(HoldOp, "hold", ResourceAccess::Change,
    results: R { snapshot: "EnvironmentType" },
    evaluate(_operands, state) {
        let snapshot = state.fp_environment;
        state.fp_environment.flags = 0;
        state.fp_environment.traps = 0;
        Ok(vec![crate::interp::Value::FpEnvironment(snapshot)])
    }
);

operation! {
    UpdateOp {
        name: "update", dialect: "fp",
        operands: O { snapshot: "EnvironmentType" },
        attributes: A { trapping: "Bool" },
        interfaces: [ResourceEffects, HasResourceSemantics, crate::interp::Interp],
        state: "in_out", verifier: "true",
    }
}

impl crate::Speculatable for RoundingConstantOp {}

fn parse_rounding(name: &str) -> Option<RoundingMode> {
    Some(match name {
        "nearest_even" => RoundingMode::TiesToEven,
        "toward_zero" => RoundingMode::TowardZero,
        "toward_negative" => RoundingMode::TowardNegative,
        "toward_positive" => RoundingMode::TowardPositive,
        "ties_away" => RoundingMode::TiesToAway,
        _ => return None,
    })
}

fn required_effects(access: ResourceAccess, memory: bool) -> Vec<(StateResource, ResourceAccess)> {
    let mut effects = vec![(StateResource::FpEnv, access)];
    effects.extend(memory.then_some((StateResource::Memory, ResourceAccess::Change)));
    effects
}

fn read_field_semantics(op: &tir::OpHandle, field_kind: ResourceField) -> ResourceSemantics {
    let context = &op.context;
    let mut graph = SemGraph::new();
    let state = resource::value(&mut graph, op, op.state_operands()[0]);
    let resource = resource::constant(&mut graph, 2, StateResource::FpEnv.semantic_code());
    let field = resource::constant(&mut graph, 2, field_kind.semantic_code());
    let value = resource::operation(&mut graph, SymKind::StateRead, &[state, resource, field]);
    let value_ty = match field_kind {
        ResourceField::FpRounding => crate::builtin::IntegerType::new(context, 3),
        ResourceField::FpFlags | ResourceField::FpTraps => {
            crate::builtin::IntegerType::new(context, 5)
        }
        ResourceField::Whole => EnvironmentType::new(context),
    };
    graph.set_actual_type(value, value_ty);
    let access = resource::constant(&mut graph, 1, ResourceAccess::Read.semantic_code());
    let observation = resource::operation(
        &mut graph,
        SymKind::StateAssign,
        &[state, resource, field, access, value],
    );
    let root = resource::operation(&mut graph, SymKind::StateResult, &[value, observation]);
    graph.set_actual_type(root, value_ty);
    ResourceSemantics {
        graph,
        root,
        value_results: vec![value],
        state_results: vec![observation],
    }
}

fn write_field_semantics(
    op: &tir::OpHandle,
    value: ValueId,
    field_kind: ResourceField,
) -> ResourceSemantics {
    let context = &op.context;
    let mut graph = SemGraph::new();
    let state = resource::value(&mut graph, op, op.state_operands()[0]);
    let resource = resource::constant(&mut graph, 2, StateResource::FpEnv.semantic_code());
    let field = resource::constant(&mut graph, 2, field_kind.semantic_code());
    let value = resource::value(&mut graph, op, value);
    let access = resource::constant(&mut graph, 1, ResourceAccess::Change.semantic_code());
    let root = resource::operation(
        &mut graph,
        SymKind::StateAssign,
        &[state, resource, field, access, value],
    );
    graph.set_actual_type(root, context.get_value(op.state_results()[0]).ty());
    ResourceSemantics {
        graph,
        root,
        value_results: Vec::new(),
        state_results: vec![root],
    }
}

impl HasResourceSemantics for GetRoundOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        read_field_semantics(&self.0, ResourceField::FpRounding)
    }
}

impl HasResourceSemantics for SetRoundOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        write_field_semantics(&self.0, self.0.operands()[0], ResourceField::FpRounding)
    }
}

impl HasResourceSemantics for ClearFlagsOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        let context = &self.0.context;
        let mut graph = SemGraph::new();
        let state = resource::value(&mut graph, &self.0, self.0.state_operands()[0]);
        let resource = resource::constant(&mut graph, 2, StateResource::FpEnv.semantic_code());
        let field = resource::constant(&mut graph, 2, ResourceField::FpFlags.semantic_code());
        let old = resource::operation(&mut graph, SymKind::StateRead, &[state, resource, field]);
        let mask = resource::value(&mut graph, &self.0, self.0.operands()[0]);
        let ones = resource::constant(&mut graph, 5, 0b1_1111);
        let inverse = resource::operation(&mut graph, SymKind::Xor, &[mask, ones]);
        let flags = resource::operation(&mut graph, SymKind::And, &[old, inverse]);
        let access = resource::constant(&mut graph, 1, ResourceAccess::Change.semantic_code());
        let root = resource::operation(
            &mut graph,
            SymKind::StateAssign,
            &[state, resource, field, access, flags],
        );
        graph.set_actual_type(root, context.get_value(self.0.state_results()[0]).ty());
        ResourceSemantics {
            graph,
            root,
            value_results: Vec::new(),
            state_results: vec![root],
        }
    }
}

impl HasResourceSemantics for GetFlagsOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        read_field_semantics(&self.0, ResourceField::FpFlags)
    }
}

impl HasResourceSemantics for GetTrapsOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        read_field_semantics(&self.0, ResourceField::FpTraps)
    }
}

impl HasResourceSemantics for SetTrapsOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        write_field_semantics(&self.0, self.0.operands()[0], ResourceField::FpTraps)
    }
}

impl HasResourceSemantics for SaveOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        read_field_semantics(&self.0, ResourceField::Whole)
    }
}

impl HasResourceSemantics for RestoreOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        write_field_semantics(&self.0, self.0.operands()[0], ResourceField::Whole)
    }
}

fn environment_state(op: &tir::OpHandle) -> ValueId {
    state_for(op, StateResource::FpEnv)
}

fn memory_state(op: &tir::OpHandle) -> ValueId {
    state_for(op, StateResource::Memory)
}

fn assign_field(
    graph: &mut SemGraph,
    state: NodeId,
    field_kind: ResourceField,
    value: NodeId,
    state_ty: crate::TypeId,
) -> NodeId {
    let resource = resource::constant(graph, 2, StateResource::FpEnv.semantic_code());
    let field = resource::constant(graph, 2, field_kind.semantic_code());
    let access = resource::constant(graph, 1, ResourceAccess::Change.semantic_code());
    let assigned = resource::operation(
        graph,
        SymKind::StateAssign,
        &[state, resource, field, access, value],
    );
    graph.set_actual_type(assigned, state_ty);
    assigned
}

fn read_field(
    graph: &mut SemGraph,
    state: NodeId,
    field_kind: ResourceField,
    ty: crate::TypeId,
) -> NodeId {
    let resource = resource::constant(graph, 2, StateResource::FpEnv.semantic_code());
    let field = resource::constant(graph, 2, field_kind.semantic_code());
    let read = resource::operation(graph, SymKind::StateRead, &[state, resource, field]);
    graph.set_actual_type(read, ty);
    read
}

fn trap_state(op: &tir::OpHandle, graph: &mut SemGraph, raised: NodeId, traps: NodeId) -> NodeId {
    let memory_value = memory_state(op);
    let memory = resource::value(graph, op, memory_value);
    let trapped = resource::operation(graph, SymKind::StateTrap, &[memory, raised, traps]);
    graph.set_actual_type(trapped, op.context.get_value(memory_value).ty());
    trapped
}

impl HasResourceSemantics for RaiseFlagsOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        let context = &self.0.context;
        let mut graph = SemGraph::new();
        let environment_value = environment_state(&self.0);
        let environment = resource::value(&mut graph, &self.0, environment_value);
        let flags_ty = crate::builtin::IntegerType::new(context, 5);
        let old = read_field(&mut graph, environment, ResourceField::FpFlags, flags_ty);
        let raised = resource::value(&mut graph, &self.0, self.0.value_operands()[0]);
        let flags = resource::operation(&mut graph, SymKind::Or, &[old, raised]);
        graph.set_actual_type(flags, flags_ty);
        let environment = assign_field(
            &mut graph,
            environment,
            ResourceField::FpFlags,
            flags,
            context.get_value(environment_value).ty(),
        );
        let memory = self.trapping().then(|| {
            let traps = read_field(&mut graph, environment, ResourceField::FpTraps, flags_ty);
            trap_state(&self.0, &mut graph, raised, traps)
        });
        transition_semantics(&self.0, None, environment, memory, graph)
    }
}

impl HasResourceSemantics for HoldOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        let context = &self.0.context;
        let mut graph = SemGraph::new();
        let environment_value = environment_state(&self.0);
        let environment = resource::value(&mut graph, &self.0, environment_value);
        let snapshot = read_field(
            &mut graph,
            environment,
            ResourceField::Whole,
            EnvironmentType::new(context),
        );
        let zero = resource::constant(&mut graph, 5, 0);
        let environment = assign_field(
            &mut graph,
            environment,
            ResourceField::FpFlags,
            zero,
            context.get_value(environment_value).ty(),
        );
        let environment = assign_field(
            &mut graph,
            environment,
            ResourceField::FpTraps,
            zero,
            context.get_value(environment_value).ty(),
        );
        transition_semantics(&self.0, Some(snapshot), environment, None, graph)
    }
}

impl HasResourceSemantics for UpdateOp {
    fn resource_semantics(&self) -> ResourceSemantics {
        let context = &self.0.context;
        let mut graph = SemGraph::new();
        let environment_value = environment_state(&self.0);
        let environment = resource::value(&mut graph, &self.0, environment_value);
        let flags_ty = crate::builtin::IntegerType::new(context, 5);
        let raised = read_field(&mut graph, environment, ResourceField::FpFlags, flags_ty);
        let snapshot = resource::value(&mut graph, &self.0, self.0.value_operands()[0]);
        let hi = resource::constant(&mut graph, 13, 4);
        let lo = resource::constant(&mut graph, 13, 0);
        let saved_flags = resource::operation(&mut graph, SymKind::Extract, &[snapshot, hi, lo]);
        graph.set_actual_type(saved_flags, flags_ty);
        let flags = resource::operation(&mut graph, SymKind::Or, &[saved_flags, raised]);
        graph.set_actual_type(flags, flags_ty);
        let environment = assign_field(
            &mut graph,
            environment,
            ResourceField::Whole,
            snapshot,
            context.get_value(environment_value).ty(),
        );
        let environment = assign_field(
            &mut graph,
            environment,
            ResourceField::FpFlags,
            flags,
            context.get_value(environment_value).ty(),
        );
        let memory = self.trapping().then(|| {
            let traps = read_field(&mut graph, environment, ResourceField::FpTraps, flags_ty);
            trap_state(&self.0, &mut graph, raised, traps)
        });
        transition_semantics(&self.0, None, environment, memory, graph)
    }
}

impl tir::Verifiable for RoundingConstantOp {
    fn verify_impl(&self, context: &Context) -> Result<(), Error> {
        if context.get_value(self.result()).ty() != RoundingType::new(context)
            || parse_rounding(&self.mode()).is_none()
        {
            return Err(Error::VerificationError(
                "fp.rounding_constant requires a known logical rounding mode".into(),
            ));
        }
        Ok(())
    }
}

macro_rules! trapping_op {
    ($op:ident) => {
        impl tir::Verifiable for $op {
            fn verify_impl(&self, _context: &Context) -> Result<(), Error> {
                verify_ports(
                    &self.0,
                    &required_effects(ResourceAccess::Change, self.trapping()),
                )
            }
        }
        impl ResourceEffects for $op {
            fn resource_effects(&self) -> Vec<ResourceEffect> {
                effect_records(
                    &self.0,
                    required_effects(ResourceAccess::Change, self.trapping()),
                )
            }
        }
    };
}

trapping_op!(RaiseFlagsOp);
trapping_op!(UpdateOp);

fn mask(value: &crate::interp::Value) -> Result<u8, crate::interp::InterpError> {
    let crate::interp::Value::Int(value) = value else {
        return Err(crate::interp::InterpError::Message(
            "flag mask must be !i5".into(),
        ));
    };
    Ok(value.to_u64() as u8)
}

impl crate::interp::Interp for RoundingConstantOp {
    fn evaluate(
        &self,
        _: &[crate::interp::Value],
        _: &mut crate::interp::ExecutionState,
    ) -> Result<Vec<crate::interp::Value>, crate::interp::InterpError> {
        Ok(vec![crate::interp::Value::Rounding(
            parse_rounding(&self.mode()).expect("verified mode"),
        )])
    }
}

impl crate::interp::Interp for RaiseFlagsOp {
    fn evaluate(
        &self,
        operands: &[crate::interp::Value],
        state: &mut crate::interp::ExecutionState,
    ) -> Result<Vec<crate::interp::Value>, crate::interp::InterpError> {
        let raised = mask(&operands[0])?;
        state.fp_environment.flags |= raised;
        trap_if_enabled(raised, self.trapping(), state)?;
        Ok(Vec::new())
    }
}

impl crate::interp::Interp for UpdateOp {
    fn evaluate(
        &self,
        operands: &[crate::interp::Value],
        state: &mut crate::interp::ExecutionState,
    ) -> Result<Vec<crate::interp::Value>, crate::interp::InterpError> {
        let [crate::interp::Value::FpEnvironment(snapshot)] = operands else {
            return Err(crate::interp::InterpError::Message(
                "fp.update requires !fp.env".into(),
            ));
        };
        let raised = state.fp_environment.flags;
        state.fp_environment = *snapshot;
        state.fp_environment.flags |= raised;
        trap_if_enabled(raised, self.trapping(), state)?;
        Ok(Vec::new())
    }
}

fn trap_if_enabled(
    raised: u8,
    trapping: bool,
    state: &crate::interp::ExecutionState,
) -> Result<(), crate::interp::InterpError> {
    if trapping && raised & state.fp_environment.traps != 0 {
        Err(crate::interp::InterpError::Message(
            "floating-point trap".into(),
        ))
    } else {
        Ok(())
    }
}
