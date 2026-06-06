use std::cell::RefCell;

use tir_be_common::sched::MachineModel;

pub enum EventHandler {
    Resource(RefCell<ResourceEventHandler>),
}

/// An event handler that prints resource utilization by instruction.
///
/// Example output:
/// ```
/// Iterations: 100
/// Instructions: 900
/// Total cycles: 500
///
/// Dispatch width: 2
/// IPC: 1.8
///
/// Instruction info:
/// [1] Latency
/// [2] RTrhoughput
/// [3] MayLoad
/// [4] MayStore
///
/// [1]   [2]   [3]  [4]   Instruction
/// 1      2               addi x1, x2, 42
/// 0      0               addi x0, x0, 0
/// 4      1               mul x5, x6, x7
///
/// Units:
/// [0] ALU0
/// [1] ALU1
/// [2] MUL0
///
/// Resource utilization per instruction:
///
/// [0]  [1]  [2]
/// 1     0    -          addi x1, x2, 42
/// -     -    -          addi x0, x0, 0
/// -     -    1          mul x5, x6, x7
/// ```
pub struct ResourceEventHandler {
    stages: Vec<&'static str>,
}

impl ResourceEventHandler {
    pub fn new(model: MachineModel) -> EventHandler {
        todo!()
    }

    pub fn notify_start(&self) {}
    pub fn notify_end(&self) {}
    pub fn notify_iteration_start(&self) {}
    pub fn notify_iteration_end(&self) {}
    pub fn instruction_entered_stage(&self, cycle: usize, id: usize, stage: &'static str) {}
    pub fn instruction_left_stage(&self, cycle: usize, id: usize, stage: &'static str) {}
    pub fn instruction_retired(&self, cycle: usize, id: usize, stage: &'static str) {}
}

impl From<ResourceEventHandler> for EventHandler {
    fn from(value: ResourceEventHandler) -> Self {
        EventHandler::Resource(RefCell::new(value))
    }
}

impl EventHandler {
    pub fn notify_start(&self) {}
    pub fn notify_end(&self) {}
    pub fn notify_iteration_start(&self) {}
    pub fn notify_iteration_end(&self) {}
    pub fn instruction_entered_stage(&self, cycle: usize, id: usize, stage: &'static str) {}
    pub fn instruction_left_stage(&self, cycle: usize, id: usize, stage: &'static str) {}
    pub fn instruction_retired(&self, cycle: usize, id: usize, stage: &'static str) {}
}
