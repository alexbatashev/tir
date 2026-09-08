//! Placement-free edits of unordered regions: put an op where its operands
//! say, grow a declared port, and drop one a loop or a gate no longer carries.

use crate::region::defining_region;
use crate::{Context, Gamma, NonLocalExit, OpId, RegionId, Theta, TypeId, ValueId};

impl Context {
    /// Put `op` into the deepest region every operand is visible from: a
    /// dependency operand pins it to that operand's region, and otherwise the
    /// innermost of the operands' regions; every other operand region must
    /// enclose or be the one chosen. Answers the region chosen. An op with no
    /// operand names no region to join, and operands of sibling regions are
    /// visible from none.
    pub fn add_auto(&self, op: OpId) -> RegionId {
        let handle = self.get_op(op);
        let regions: Vec<RegionId> = handle
            .operands()
            .iter()
            .filter_map(|&value| defining_region(self, value))
            .collect();
        let chosen = match handle.dep_operands().first() {
            Some(&dep) => defining_region(self, dep).expect("a dependency is defined in a region"),
            None => regions
                .iter()
                .copied()
                .max_by_key(|&region| self.region_ancestors(region).len())
                .expect("an operation with no operand names no region to join"),
        };
        let chain = self.region_ancestors(chosen);
        assert!(
            regions.iter().all(|region| chain.contains(region)),
            "operands of sibling regions are visible from no region"
        );
        self.add(chosen, op);
        chosen
    }

    /// `region` and every region enclosing it, innermost first.
    fn region_ancestors(&self, region: RegionId) -> Vec<RegionId> {
        let mut chain = vec![region];
        let mut current = region;
        while let Some(parent) = self
            .get_region(current)
            .parent_op()
            .and_then(|op| self.region_of_op(op))
        {
            chain.push(parent);
            current = parent;
        }
        chain
    }

    /// Whether `op` is one of `roots` or sits under one, at any depth.
    fn op_under(&self, op: OpId, roots: &[OpId]) -> bool {
        let mut current = Some(op);
        while let Some(op) = current {
            if roots.contains(&op) {
                return true;
            }
            current = self.parent_op(op);
        }
        false
    }

    /// Every op under `roots`, the roots included.
    fn subtree_ops(&self, roots: &[OpId]) -> Vec<OpId> {
        let mut found = roots.to_vec();
        let mut index = 0;
        while index < found.len() {
            for region in self.get_op(found[index]).regions() {
                found.extend(self.get_region(region).op_ids());
            }
            index += 1;
        }
        found
    }

    /// `region` and every region nested in it.
    pub(crate) fn nested_regions(&self, region: RegionId) -> Vec<RegionId> {
        let ops = self.get_region(region).op_ids();
        let mut regions = vec![region];
        regions.extend(
            self.subtree_ops(&ops)
                .iter()
                .flat_map(|&op| self.get_op(op).regions()),
        );
        regions
    }

    /// Region results sit in no use list; rename `old` to `new` in the result
    /// list of `region` and every region nested in it, except regions under
    /// `except`, which keep naming the value they define.
    pub(crate) fn rename_region_results(
        &self,
        region: RegionId,
        old: ValueId,
        new: ValueId,
        except: &[OpId],
    ) {
        for nested in self.nested_regions(region) {
            let handle = self.get_region(nested);
            if handle
                .parent_op()
                .is_some_and(|owner| self.op_under(owner, except))
            {
                continue;
            }
            let mut results = handle.results();
            if results.contains(&old) {
                let deps = handle.dep_results().len();
                for result in &mut results {
                    if *result == old {
                        *result = new;
                    }
                }
                self.set_region_results(nested, results, deps);
            }
        }
    }

    /// [`Context::rename_region_results`] for a set of renames, in one walk:
    /// finding the nested regions costs the whole subtree, so a rename per
    /// value is quadratic in the operations under `region`. A result named by
    /// `renames` follows the chain to the value nothing renames.
    pub(crate) fn rename_region_results_batch(
        &self,
        region: RegionId,
        renames: &std::collections::HashMap<ValueId, ValueId>,
    ) {
        if renames.is_empty() {
            return;
        }
        for nested in self.nested_regions(region) {
            let handle = self.get_region(nested);
            let mut results = handle.results();
            let mut renamed = false;
            for result in &mut results {
                while let Some(&next) = renames.get(result) {
                    *result = next;
                    renamed = true;
                }
            }
            if renamed {
                let deps = handle.dep_results().len();
                self.set_region_results(nested, results, deps);
            }
        }
    }

    /// The non-local exits under `roots` that leave `target`.
    fn exits_leaving(&self, roots: &[OpId], target: OpId) -> Vec<OpId> {
        self.subtree_ops(roots)
            .into_iter()
            .filter(|&op| {
                self.get_op(op).has_interface::<dyn NonLocalExit>()
                    && crate::analysis::exits::resolve_exit_target(self, op).ok() == Some(target)
            })
            .collect()
    }

    /// [`Context::grow_port`] for an op with a declared binding: one more
    /// carried value, or dependency when `dependency`, at the end of every
    /// aligned range. A loop's exit value defaults to the port; a gamma's arms
    /// must each name what they produce for it. Every non-local exit that
    /// leaves `op` gains the port of the region it sits in.
    pub(crate) fn grow_declared_port(
        &self,
        op: OpId,
        ty: TypeId,
        init: Option<ValueId>,
        mut latch: impl FnMut(RegionId, Option<ValueId>) -> Option<ValueId>,
        dependency: bool,
    ) -> ValueId {
        let handle = self.get_op(op);
        let deps_in = handle.dep_operands().len();
        let port_of = |region: RegionId, index: usize| {
            let port = self.create_value(ty, None);
            self.insert_region_port(region, index, port.clone(), dependency);
            port.id()
        };
        let feed = |region: RegionId, port: ValueId| {
            for exit in self.exits_leaving(&self.get_region(region).op_ids(), op) {
                if dependency {
                    self.append_dep_operand(exit, port);
                } else {
                    self.append_operand(exit, port);
                }
            }
        };
        if let Some(theta) = handle.clone().as_interface::<dyn Theta>() {
            let body = theta.body();
            let binding = theta.carried();
            let init = init.expect("a loop port carries a value in");
            let (operands, ports, continue_, exit) = if dependency {
                (deps_in, deps_in, deps_in, 2 * deps_in)
            } else {
                (
                    binding.operands.end,
                    binding.ports.end,
                    binding.continue_.end,
                    binding.exit.end,
                )
            };
            self.insert_operand_at(op, operands, init, dependency);
            let port = port_of(body, ports);
            let carried = latch(body, Some(port)).unwrap_or(port);
            self.insert_region_result(body, exit, port, dependency);
            self.insert_region_result(body, continue_, carried, dependency);
            feed(body, port);
        } else if let Some(gamma) = handle.clone().as_interface::<dyn Gamma>() {
            let arms = gamma.arms();
            let binding = gamma.forwarded();
            let (operands, ports, joined) = if dependency {
                (deps_in, deps_in, handle.dep_results().len())
            } else {
                (binding.operands.end, binding.ports.end, binding.exit.end)
            };
            if let Some(init) = init {
                self.insert_operand_at(op, operands, init, dependency);
            }
            for &arm in &arms {
                let port = init.map(|_| port_of(arm, ports));
                let produced = latch(arm, port).expect("every arm produces the port's value");
                self.insert_region_result(arm, joined, produced, dependency);
                if let Some(port) = port {
                    feed(arm, port);
                }
            }
        } else {
            panic!("grow_declared_port needs a Theta or Gamma");
        }
        if dependency {
            self.append_dep_result(op)
        } else {
            self.append_result(op, ty)
        }
    }

    /// Drop the dependency port a loop or a gate carries at `index`, the
    /// inverse of [`Context::grow_dep_port`]. The chain it carried flows past
    /// the operation instead of through it, which is what a chain the body
    /// leaves alone was doing all along.
    ///
    /// The caller has already handed the port's readers what the operation was
    /// entered on and its result's readers that same state; what is left is the
    /// port list, and it goes.
    pub fn drop_dep_port(&self, op: OpId, index: usize) {
        let regions = self.get_op(op).regions().to_vec();
        for &region in &regions {
            let handle = self.get_region(region);
            let ports = handle.dep_arguments().len();
            let mut results = handle.results();
            let deps = handle.dep_results().len();
            let groups = deps.checked_div(ports).unwrap_or(0);
            let values = results.len() - deps;
            for group in (0..groups).rev() {
                results.remove(values + group * ports + index);
            }
            self.set_region_results(region, results, deps - groups);
            self.remove_region_dep_port(region, index);
        }
        self.remove_dep_operand(op, index);
        self.remove_dep_result(op, index);
    }
}
