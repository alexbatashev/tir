//! What an operation does to the memory it names, and where it names it.

use crate::builtin::StateResource;
use crate::{MemoryRead, MemoryWrite, OpHandle, ResourceAccess, ResourceEffects, ValueId};

/// What one operation does to the memory it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Observes a memory and leaves it as it found it.
    Read,
    /// Leaves a memory at an address that the reads after it see.
    Change,
}

/// What `op` declares it does to memory.
pub fn effect_of(op: &OpHandle) -> Option<Effect> {
    resource_effect(op, StateResource::Memory).map(|effect| match effect.access {
        ResourceAccess::Read => Effect::Read,
        ResourceAccess::Change => Effect::Change,
    })
}

pub fn resource_effect(op: &OpHandle, resource: StateResource) -> Option<crate::ResourceEffect> {
    op.clone()
        .as_interface::<dyn ResourceEffects>()?
        .resource_effects()
        .into_iter()
        .find(|effect| effect.resource == resource)
}

/// One declared access: where it lands, the value it names there, the state it
/// observes, and whether it puts the value in memory or takes it out.
pub struct Access {
    pub location: ValueId,
    pub value: ValueId,
    pub state: Option<ValueId>,
    pub write: bool,
}

/// The access `op` declares, read off whichever memory interface it carries.
pub fn access_of(op: &OpHandle) -> Option<Access> {
    if let Some(write) = op.clone().as_interface::<dyn MemoryWrite>() {
        return Some(Access {
            location: write.write_location(),
            value: write.written_value(),
            state: observed_state(op),
            write: true,
        });
    }
    let read = op.clone().as_interface::<dyn MemoryRead>()?;
    Some(Access {
        location: read.read_location(),
        value: read.read_value(),
        state: observed_state(op),
        write: false,
    })
}

/// The one state `op` observes, if it has been put on a chain.
pub fn observed_state(op: &OpHandle) -> Option<ValueId> {
    resource_effect(op, StateResource::Memory)?
        .observed
        .first()
        .copied()
}

/// The one state `op` leaves behind, if it has been put on a chain.
pub fn produced_state(op: &OpHandle) -> Option<ValueId> {
    resource_effect(op, StateResource::Memory)?
        .produced
        .first()
        .copied()
}
