//! Seeds the e-graph by reading a function's regions. Gates come off the ops'
//! own interfaces, never from control-flow analysis: a [`Conditional`] result is
//! the γ over the values its arms yield, and an arm's entry arguments are the
//! inputs forwarded into it. A memory access is the `LoadMemory`/`StoreMemory`
//! term over the state it reads, so the chain is an ordinary edge. Everything
//! else the vocabulary cannot spell — a loop's carried port and results, a
//! multi-result or effectful op — anchors as an input leaf.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tir_symbolic::egraph::{EGraph, Id};

use crate::analysis::slots::{collect_slots, values_agree_on_type};
use crate::builtin::StateType;
use crate::sem::egraph::{minimal_unsigned_apint, type_width};
use crate::sem::{Prov, SemNode as Node, SymKind};
use crate::{
    BlockId, Commutative, Conditional, ConstantLike, Context, MemoryRead, MemoryWrite, OpId,
    OpInstance, RegionId, TypeId, ValueId,
};

/// The operands a store term names: the location, the value it writes, and the
/// state it observes.
const STORE_OPERANDS: usize = 3;

/// The seeded e-graph plus the driver's maps: each value's class, each block
/// argument's block, the state classes something outside the term graph
/// observes, and the addresses a law may reason about as a value.
pub struct Seeded {
    pub eg: EGraph<Node>,
    pub value_class: HashMap<ValueId, Id>,
    pub arg_block: HashMap<ValueId, BlockId>,
    pub exported_states: Vec<Id>,
    pub promotable_addresses: Vec<Id>,
}

/// Build the e-graph for the regions of `root`.
pub fn seed(context: &Context, root: OpId) -> Seeded {
    let mut seeder = Seeder {
        context,
        eg: EGraph::new(),
        value_class: HashMap::new(),
        arg_block: HashMap::new(),
        seeded: HashSet::new(),
        state_ty: StateType::new(context),
        exported_states: Vec::new(),
        ops: Vec::new(),
    };
    for region in context.get_op(root).regions.clone() {
        seeder.seed_region(region);
    }
    let promotable_addresses = seeder.promotable_addresses();
    Seeded {
        eg: seeder.eg,
        value_class: seeder.value_class,
        arg_block: seeder.arg_block,
        exported_states: seeder.exported_states,
        promotable_addresses,
    }
}

struct Seeder<'a> {
    context: &'a Context,
    eg: EGraph<Node>,
    value_class: HashMap<ValueId, Id>,
    arg_block: HashMap<ValueId, BlockId>,
    seeded: HashSet<OpId>,
    state_ty: TypeId,
    exported_states: Vec<Id>,
    ops: Vec<OpId>,
}

impl Seeder<'_> {
    fn seed_region(&mut self, region: RegionId) {
        let blocks: Vec<_> = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .collect();
        for block in blocks {
            for argument in block.arguments() {
                self.arg_block.insert(argument.id(), block.id());
                self.class_of(argument.id());
            }
            for op in block.op_ids() {
                self.ops.push(op);
                self.seed_op(op);
            }
        }
    }

    /// The classes of the addresses a slot-level law may treat as a value: an
    /// allocation whose pointer never leaves the load/store locations, and whose
    /// accesses agree on one type, so a value reconstructed for it has one
    /// spelling. The reading is the one every memory transform asks for.
    fn promotable_addresses(&self) -> Vec<Id> {
        collect_slots(self.context, &self.ops)
            .iter()
            .filter(|(_, slot)| {
                slot.alloca.is_some() && !slot.escapes && values_agree_on_type(self.context, slot)
            })
            .filter_map(|(address, _)| self.value_class.get(address).copied())
            .collect()
    }

    /// The class of `value`: the one already seeded for it, the one its defining
    /// op seeds, or an anchor leaf.
    fn class_of(&mut self, value: ValueId) -> Id {
        if let Some(&id) = self.value_class.get(&value) {
            return id;
        }
        if let Some(op) = self.context.get_value(value).defining_op() {
            self.seed_op(op);
        }
        self.anchor(value)
    }

    /// The leaf standing for `value`, unless something already seeded a class for it.
    fn anchor(&mut self, value: ValueId) -> Id {
        if let Some(&id) = self.value_class.get(&value) {
            return id;
        }
        let id = self.eg.add(Node::input(value));
        self.value_class.insert(value, id);
        id
    }

    /// Seed every result of `op`. Memoized: an operand whose definition the walk
    /// has not reached yet pulls it in, and the memo breaks any cycle it meets.
    fn seed_op(&mut self, op: OpId) {
        if !self.seeded.insert(op) {
            return;
        }
        let instance = self.context.get_op(op);

        if !instance.regions.is_empty() {
            self.export_states(&instance);
            if let Some(conditional) = instance.clone().as_interface::<dyn Conditional>() {
                self.seed_gamma(&instance, conditional.as_ref());
                return;
            }
            // A loop carries no reasoning yet: its ports and results anchor, but
            // its body is read like any other region.
            for region in instance.regions.clone() {
                self.seed_region(region);
            }
        } else if let (Some(constant), [result]) = (
            instance.clone().as_interface::<dyn ConstantLike>(),
            &instance.results[..],
        ) {
            let ty = self.context.get_value(*result).ty();
            let id = self
                .eg
                .add(Node::constant(constant.constant_value(), Prov::Op(op)).typed(ty));
            self.value_class.insert(*result, id);
            return;
        } else if self.seed_memory(&instance) {
            return;
        } else if is_pure_value(&instance) {
            let value = instance.results[0];
            let ty = self.context.get_value(value).ty();
            let commutative = instance.has_interface::<dyn Commutative>();
            let mut args: Vec<Id> = instance
                .operands
                .clone()
                .iter()
                .map(|&operand| self.class_of(operand))
                .collect();
            if commutative {
                args.sort_by_key(|id| id.index());
            }
            let id = self.eg.add(Node::seeded(&instance, ty, commutative, args));
            self.value_class.insert(value, id);
            return;
        } else {
            self.export_states(&instance);
        }

        for result in instance.results.clone() {
            self.anchor(result);
        }
    }

    /// γ: each arm's entry arguments bind to the inputs forwarded into it, and
    /// result `index` is the choice between the arms' `index`-th yielded values.
    fn seed_gamma(&mut self, instance: &Arc<OpInstance>, conditional: &dyn Conditional) {
        let decision = self.class_of(conditional.decision());
        for region in instance.regions.clone() {
            self.bind_arm_arguments(instance, region);
            self.seed_region(region);
        }
        for (index, &result) in instance.results.clone().iter().enumerate() {
            let Some((taken, not_taken)) = arm_yields(conditional, index) else {
                self.anchor(result);
                continue;
            };
            let args = vec![decision, self.class_of(taken), self.class_of(not_taken)];
            let id = self.eg.add(Node::gamma(result, args));
            self.value_class.insert(result, id);
        }
    }

    /// An arm's entry arguments are the inputs the gate forwards into it: the
    /// operation's trailing operands, one per argument.
    fn bind_arm_arguments(&mut self, instance: &Arc<OpInstance>, region: RegionId) {
        let Some(block) = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .next()
        else {
            return;
        };
        let arguments: Vec<ValueId> = block.arguments().iter().map(|a| a.id()).collect();
        let first = instance.operands.len().saturating_sub(arguments.len());
        for (&argument, &input) in arguments.iter().zip(&instance.operands[first..]) {
            let id = self.class_of(input);
            self.value_class.insert(argument, id);
        }
    }

    /// Record the states `instance` observes as exported. The term graph models a
    /// state only through the accesses on it, so a state any other operation takes
    /// — returned, yielded out of a region, handed to a call — is observed by
    /// something the graph cannot see, and no law may drop the write that left it.
    fn export_states(&mut self, instance: &Arc<OpInstance>) {
        for operand in instance.operands.clone() {
            if self.context.get_value(operand).ty() == self.state_ty {
                let id = self.class_of(operand);
                self.exported_states.push(id);
            }
        }
    }

    /// Seed a memory access over the state it reads, if it is one that names a state.
    fn seed_memory(&mut self, instance: &Arc<OpInstance>) -> bool {
        if let Some(read) = instance.clone().as_interface::<dyn MemoryRead>() {
            return self.seed_read(read.as_ref());
        }
        if let Some(write) = instance.clone().as_interface::<dyn MemoryWrite>() {
            return self.seed_write(instance, write.as_ref());
        }
        false
    }

    /// A read leaves memory as it found it, so the state it publishes is the one
    /// it read: both name the same class, and two reads of one address in one
    /// state are one term.
    fn seed_read(&mut self, read: &dyn MemoryRead) -> bool {
        let value = read.read_value();
        let ty = self.context.get_value(value).ty();
        let (Some(bits), Some(state)) = (type_width(self.context, ty), read.state_operand()) else {
            return false;
        };
        let address = self.class_of(read.read_location());
        let bytes = self.int(bits / 8);
        let metadata = self.int(0);
        let state = self.class_of(state);
        let id = self.eg.add(
            Node::access(
                SymKind::LoadMemory,
                value,
                vec![address, bytes, metadata, state],
            )
            .typed(ty),
        );
        self.value_class.insert(value, id);
        if let Some(published) = read.state_result() {
            self.value_class.insert(published, state);
        }
        true
    }

    /// A write's term *is* the state the accesses after it read. The term names
    /// the location, the value and the state, so a write carrying any other
    /// operand — a size, say — covers an extent the term cannot spell and is no
    /// store term at all.
    fn seed_write(&mut self, instance: &Arc<OpInstance>, write: &dyn MemoryWrite) -> bool {
        if instance.operands.len() != STORE_OPERANDS {
            return false;
        }
        let written = write.written_value();
        let ty = self.context.get_value(written).ty();
        let (Some(bits), Some(state), Some(published)) = (
            type_width(self.context, ty),
            write.state_operand(),
            write.state_result(),
        ) else {
            return false;
        };
        let address = self.class_of(write.write_location());
        let bytes = self.int(bits / 8);
        let value = self.class_of(written);
        let address_space = self.int(0);
        let state = self.class_of(state);
        let id = self.eg.add(Node::access(
            SymKind::StoreMemory,
            published,
            vec![address, bytes, value, address_space, state],
        ));
        self.value_class.insert(published, id);
        true
    }

    /// A byte count or metadata literal, spelled the one way the vocabulary shares.
    fn int(&mut self, value: u32) -> Id {
        self.eg.add(Node::constant(
            minimal_unsigned_apint(u64::from(value)),
            Prov::None,
        ))
    }
}

/// The values a two-armed boolean [`Conditional`]'s true and false arms yield at
/// `index`. `None` for any other shape — a switch, or a result no arm yields.
fn arm_yields(conditional: &dyn Conditional, index: usize) -> Option<(ValueId, ValueId)> {
    let mut taken = None;
    let mut not_taken = None;
    for (region, _, when_true) in conditional.guarded_regions() {
        let yielded = conditional.region_yields(region).get(index).copied()?;
        if when_true {
            taken = Some(yielded);
        } else {
            not_taken = Some(yielded);
        }
    }
    Some((taken?, not_taken?))
}

/// A pure value op the e-graph may reason about: one result, no regions, and a declared semantic expression.
fn is_pure_value(instance: &Arc<OpInstance>) -> bool {
    instance.results.len() == 1
        && instance.regions.is_empty()
        && instance
            .clone()
            .as_dyn_op()
            .semantic_expr(&mut crate::sem::SemGraph::new())
            .is_some()
}
