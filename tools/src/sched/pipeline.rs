use tir::builtin::ModuleOp;
use tir_be_common::sched::MachineModel;

use crate::sched::event::EventHandler;

pub struct Pipeline {
    model: MachineModel,
    events: EventHandler,
}

impl Pipeline {
    pub fn run(mut self, context: &Context, instrs: ModuleOp, max_cycles: usize, num_iters: usize) {
        self.events.notify_start();
        let mut cycle = 0;

        for iter in 0..num_iters {
            self.events.notify_iteration_start();

            // iterate pipeline stages in reverse order

            cycle += 1;

            if cycle >= max_cycles {
                break;
            }
            self.events.notify_iteration_end();
        }

        self.events.notify_end();
    }
}
