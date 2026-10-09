//! The cover of a region, found by folding costs over its classes.
//!
//! A class takes the instance that adds the least to the region: its own
//! cost and that of every instance its inputs would need that nothing else
//! already pays for, each counted once however many paths reach it. Classes
//! are decided from the readers down, so an instance performing an effect
//! inside itself is chosen before the effect would take one of its own.
//!
//! Nothing is recorded ahead of the fold. A class is met when an instance
//! that could win reads it; its matches are then tried cheapest rule first,
//! and the first whose rule alone costs more than the best instance so far
//! ends the search. Only the instances tried are checked against the region,
//! and only the ones chosen are written to its record.
//!
//! The fold decides each class once, so it can miss a cover that exists. It
//! answers `None` then. The caller checks whatever it does answer against
//! the record.

use smallvec::SmallVec;
use tir::Context;
use tir_egraph::Ref;

use super::cover::{BoundaryDemand, ChildDemand, InstanceKey, PbqpIselAlternative, PbqpIselMatch};
use super::matches::Matches;
use super::pattern::Found;
use super::problem::{ControlChoice, HasRegister, RegionProblem};
use super::{FunctionSelection, InstructionSelectPass};
use crate::utils::fx::{HashMap, HashSet};

/// What orders instances of one cost: specificity, whether the instance
/// constrains anything, and the registers it reads.
type Rank = (std::cmp::Reverse<usize>, bool, usize);

/// The longest chain of classes, each read by the next alone, the fold
/// follows.
const DEPTH: u32 = 512;

/// The instance a class would take were it demanded in a register, and what
/// that adds.
#[derive(Clone, Copy)]
enum Choice {
    Unknown,
    /// Being decided: a class reached again through its own inputs.
    Open,
    Known(Option<(usize, u64)>),
}

struct Site {
    class: Ref,
    /// The matches at the class, cheapest rule first, once asked for.
    raws: Option<Vec<(usize, Found)>>,
    /// Per match, the instance it is in this region, once tried.
    tried: Vec<Option<Option<usize>>>,
    demanded: bool,
    available: bool,
    /// An instance may perform the class inside itself.
    coverable: bool,
    choice: Choice,
    /// The last pricing walk that counted the site.
    counted: u32,
    state: Option<PbqpIselAlternative>,
}

struct Fold<'a> {
    pass: &'a InstructionSelectPass,
    context: &'a Context,
    fs: &'a FunctionSelection,
    value_matches: &'a Matches,
    problem: &'a mut RegionProblem,
    /// Every instance tried, chosen or not. Only the chosen ones reach the
    /// region's record; the storage is the function's, reused by each region.
    tried: Vec<PbqpIselMatch>,
    has_register: HasRegister<'a>,
    guard_classes: HashSet<Ref>,
    overlay: HashSet<Ref>,
    waived: HashSet<Ref>,
    index: HashMap<Ref, usize>,
    sites: Vec<Site>,
    /// The pricing walks made so far.
    walks: u32,
    /// How many classes deep the choice being made is.
    depth: u32,
}

impl InstructionSelectPass {
    /// Fold a cover of `problem` under `choices`, writing the classes met
    /// and the instances tried into it. The instance chosen at each class
    /// it roots, or `None` where the fold finds no cover.
    pub(super) fn fold_region(
        &self,
        context: &Context,
        fs: &FunctionSelection,
        problem: &mut RegionProblem,
        value_matches: &Matches,
        choices: &[ControlChoice],
        has_register: HasRegister,
    ) -> Option<HashMap<Ref, usize>> {
        let mut overlay = HashSet::default();
        let mut waived = HashSet::default();
        for (control, choice) in problem.controls.iter().zip(choices) {
            match *choice {
                ControlChoice::Decided(_) => {}
                ControlChoice::Fused(index) => {
                    overlay.extend(problem.guard_demands(&control.fused[index]));
                    waived.extend(control.waives.iter().copied());
                }
                ControlChoice::Nonzero => {
                    overlay.insert(problem.facts(control.condition).source);
                }
            }
        }
        let seeds = std::mem::take(&mut problem.classes);
        let guard_classes = fs.aux_classes(problem.region).collect();
        let mut fold = Fold {
            pass: self,
            context,
            fs,
            value_matches,
            problem,
            tried: std::mem::take(&mut *fs.tried.borrow_mut()),
            has_register,
            guard_classes,
            overlay,
            waived,
            index: HashMap::default(),
            sites: Vec::new(),
            walks: 0,
            depth: 0,
        };
        let covered = fold.cover(&seeds);
        let mut classes: Vec<Ref> = fold.sites.iter().map(|site| site.class).collect();
        classes.sort();
        let chosen: Vec<(Ref, usize)> = fold
            .sites
            .iter()
            .filter_map(|site| match site.state {
                Some(PbqpIselAlternative::Tile { match_id }) => Some((site.class, match_id)),
                _ => None,
            })
            .collect();
        let mut tried = std::mem::take(&mut fold.tried);
        problem.classes = classes;
        // Matches of one rule's result patterns over the same operands are
        // the results of one instance.
        let mut instances: HashMap<InstanceKey, usize> = HashMap::default();
        let mut tiles = HashMap::default();
        for (class, match_id) in chosen {
            let index = problem.matches.len();
            let mut matched = tried[match_id].clone();
            matched.instance = if self.rules[matched.rule_index].secondary.is_empty() {
                index
            } else {
                let mut operands = matched.bindings.captures.entries.clone();
                operands.sort_unstable();
                *instances
                    .entry((matched.rule_index, operands))
                    .or_insert(index)
            };
            problem.matches.push(matched);
            tiles.insert(class, index);
        }
        tried.clear();
        *fs.tried.borrow_mut() = tried;
        covered.then_some(tiles)
    }
}

impl Fold<'_> {
    fn cover(&mut self, seeds: &[Ref]) -> bool {
        for &seed in seeds {
            self.site(seed);
        }
        // Readers first: a later operation of the region reads an earlier one.
        let rank = |fold: &Self, site: usize| {
            let position = fold
                .problem
                .source_op(fold.sites[site].class)
                .map(|op| fold.fs.scopes.position[&op]);
            std::cmp::Reverse((position.is_none(), position))
        };
        let mut roots: Vec<usize> = (0..self.sites.len())
            .filter(|&site| self.sites[site].demanded && !self.sites[site].available)
            .collect();
        roots.sort_by_key(|&site| rank(self, site));
        for root in roots {
            if self.sites[root].state.is_some() {
                continue;
            }
            // A demanded source no operation computes may leave each of its
            // low-bit views to an instance of its own.
            let class = self.sites[root].class;
            let views: Vec<usize> = match self.problem.facts(class).has_values {
                true => Vec::new(),
                false => (0..self.sites.len())
                    .filter(|&site| {
                        let view = self.sites[site].class;
                        view != class && self.problem.facts(view).source == class
                    })
                    .collect(),
            };
            let own = self.choice(root).map(|(_, cost)| cost);
            let deferred = (!views.is_empty())
                .then(|| {
                    views.iter().try_fold(0u64, |sum, &view| {
                        Some(sum.saturating_add(self.best(view, None)?.1))
                    })
                })
                .flatten();
            let decided = match deferred {
                // At equal cost the views are computed: each is the instruction
                // its own width asks for.
                Some(deferred) if own.is_none_or(|own| deferred <= own) => {
                    self.sites[root].state = Some(PbqpIselAlternative::Deferred);
                    views.into_iter().all(|view| self.select(view, None, true))
                }
                _ => self.select(root, None, false),
            };
            if !decided {
                return false;
            }
        }
        true
    }

    /// The site of `class`, met now if it has none: its facts are read, and
    /// the source a view of it reads is met with it.
    fn site(&mut self, class: Ref) -> usize {
        if let Some(&site) = self.index.get(&class) {
            return site;
        }
        self.pass.record_facts(
            self.context,
            self.fs,
            self.problem.region,
            &mut self.problem.facts,
            class,
        );
        let demanded = |fold: &Self, class: Ref| {
            let overlaid = fold.overlay.contains(&class);
            (!fold.waived.contains(&class) || overlaid)
                && (overlaid || fold.problem.facts(class).placed)
        };
        let facts = self.problem.facts(class);
        let source = facts.source;
        let held = self.problem.facts(source);
        let site = Site {
            class,
            raws: None,
            tried: Vec::new(),
            demanded: demanded(self, class),
            available: held.has_register(self.has_register)
                || held.hook_constant
                || (source != class && demanded(self, source)),
            coverable: !(self.overlay.contains(&class) || facts.register_demand),
            choice: Choice::Unknown,
            counted: 0,
            state: None,
        };
        let at = self.sites.len();
        self.sites.push(site);
        self.index.insert(class, at);
        if source != class {
            self.site(source);
        }
        at
    }

    /// The instance the match `raw` of `site` is here, if the region may
    /// select it, built the first time it is asked for.
    fn instance(&mut self, site: usize, raw: usize) -> Option<usize> {
        if let Some(tried) = self.sites[site].tried[raw] {
            return tried;
        }
        let (pattern, found) = &self.sites[site].raws.as_ref().expect("matches read")[raw];
        let instance = self.pass.instance(
            self.context,
            self.fs,
            &self.problem.source_ops,
            &self.guard_classes,
            *pattern,
            found,
        );
        let match_id = instance.map(|matched| {
            for &(_, class) in &matched.bindings.captures.entries {
                self.pass.record_facts(
                    self.context,
                    self.fs,
                    self.problem.region,
                    &mut self.problem.facts,
                    class,
                );
            }
            self.tried.push(matched);
            self.tried.len() - 1
        });
        self.sites[site].tried[raw] = Some(match_id);
        match_id
    }

    /// The instance the class takes when nothing constrains the register it
    /// defines, with what it adds; `None` where it has none.
    fn choice(&mut self, site: usize) -> Option<(usize, u64)> {
        match self.sites[site].choice {
            Choice::Known(choice) => return choice,
            Choice::Open => return None,
            Choice::Unknown => {}
        }
        // A chain of single readers this long is left to the solver, which
        // does not recurse along it. It also ends the descent through a class
        // that holds `x + k` for itself, whose every offset reads another.
        if self.depth == DEPTH {
            return None;
        }
        self.sites[site].choice = Choice::Open;
        self.depth += 1;
        let choice = self.best(site, None);
        self.depth -= 1;
        self.sites[site].choice = Choice::Known(choice);
        choice
    }

    /// Whether something other than a reader already pays for the class in a
    /// register: one holds it, an instance was chosen for it, or the region
    /// computes it whoever reads it. An effect is never paid for ahead of
    /// its reader, since an instance performing it saves the one that would.
    fn paid(&self, site: usize) -> bool {
        let at = &self.sites[site];
        at.available
            || matches!(at.state, Some(PbqpIselAlternative::Tile { .. }))
            || (at.demanded && self.problem.facts(at.class).pure)
    }

    /// The cheapest instance at the class a reader demanding `demand`
    /// accepts, with its cost.
    fn best(&mut self, site: usize, demand: Option<&ChildDemand>) -> Option<(usize, u64)> {
        if self.sites[site].raws.is_none() {
            let mut raws =
                self.pass
                    .raw_matches(self.fs, self.value_matches, self.sites[site].class);
            raws.sort_by_key(|&(pattern, _)| self.rule_cost(pattern));
            self.sites[site].tried = vec![None; raws.len()];
            self.sites[site].raws = Some(raws);
        }
        // Of equals the more specific wins, then one that constrains nothing,
        // then the one reading fewer registers: an operand folded into the
        // encoding holds none.
        let mut best: Option<(usize, u64, Rank)> = None;
        for raw in 0..self.sites[site].tried.len() {
            let pattern = self.sites[site].raws.as_ref().expect("matches read")[raw].0;
            // No instance costs less than its rule.
            if best.is_some_and(|(_, least, _)| self.rule_cost(pattern) > least) {
                break;
            }
            let Some(match_id) = self.instance(site, raw) else {
                continue;
            };
            if demand.is_some_and(|demand| !self.answers(demand, match_id)) {
                continue;
            }
            let Some(cost) = self.instance_cost(match_id) else {
                continue;
            };
            let registers = self.tried[match_id]
                .uses
                .iter()
                .filter(|(_, demand)| matches!(demand, ChildDemand::Register { .. }))
                .count();
            let matched = &self.tried[match_id];
            let constrains = matched.bindings.pattern_nodes.iter().any(|binding| {
                !(binding.is_state
                    || (binding.pattern_node == matched.pattern_root
                        && binding.class == matched.root)
                    || (binding.is_boundary && binding.demand == BoundaryDemand::Structural))
            });
            let rank = (
                std::cmp::Reverse(self.pass.specificity[pattern]),
                constrains,
                registers,
            );
            if best.is_none_or(|(_, least, ahead)| (cost, rank) < (least, ahead)) {
                best = Some((match_id, cost, rank));
            }
        }
        best.map(|(match_id, cost, _)| (match_id, cost))
    }

    fn rule_cost(&self, pattern: usize) -> u64 {
        u64::from(self.pass.rules[self.pass.compiled_patterns[pattern].rule_index].base_cost)
    }

    /// Whether the instance `match_id` defines the register `demand` reads.
    fn answers(&self, demand: &ChildDemand, match_id: usize) -> bool {
        demand.accepts(
            usize::MAX,
            &PbqpIselAlternative::Tile { match_id },
            &self.tried,
            false,
            false,
        )
    }

    /// Whether a register that already holds a class answers `demand`.
    fn held(&self, demand: &ChildDemand) -> bool {
        demand.accepts(
            usize::MAX,
            &PbqpIselAlternative::NotDemanded,
            &self.tried,
            false,
            true,
        )
    }

    /// What selecting the instance adds to the region: its cost and that of
    /// every instance its register inputs would take that nothing pays for
    /// yet, each once. `None` where an input cannot be met.
    fn instance_cost(&mut self, match_id: usize) -> Option<u64> {
        let root = self.tried[match_id].root;
        let mut inputs: SmallVec<[usize; 8]> = SmallVec::new();
        for at in 0..self.tried[match_id].uses.len() {
            let (class, demand) = self.tried[match_id].uses[at].clone();
            match &demand {
                ChildDemand::Register { .. } => {
                    let child = self.site(class);
                    match &self.sites[child].state {
                        Some(PbqpIselAlternative::Tile { match_id }) => {
                            if !self.answers(&demand, *match_id) {
                                return None;
                            }
                        }
                        Some(PbqpIselAlternative::NotDemanded) | None
                            if self.sites[child].available =>
                        {
                            if !self.held(&demand) {
                                return None;
                            }
                        }
                        Some(_) => return None,
                        None => {
                            self.choice(child)?;
                            inputs.push(child);
                        }
                    }
                }
                ChildDemand::Immediate => {
                    self.pass.record_facts(
                        self.context,
                        self.fs,
                        self.problem.region,
                        &mut self.problem.facts,
                        class,
                    );
                    self.problem.facts(class).int.as_ref()?;
                }
                ChildDemand::Effect => {
                    let child = self.site(class);
                    let site = &self.sites[child];
                    let free = !site.demanded || site.available;
                    match &site.state {
                        None if free || site.coverable => {}
                        Some(PbqpIselAlternative::NotDemanded) => {}
                        Some(PbqpIselAlternative::CoveredBy { match_id: owner })
                            if self.tried[*owner].root == root => {}
                        _ => return None,
                    }
                }
                ChildDemand::None => {}
            }
        }
        // Every input has its choice by now, so the walk only reads them.
        self.walks += 1;
        let walk = self.walks;
        let mut cost = self.tried[match_id].cost;
        while let Some(site) = inputs.pop() {
            if self.sites[site].counted == walk || self.paid(site) {
                continue;
            }
            self.sites[site].counted = walk;
            let Choice::Known(Some((chosen, _))) = self.sites[site].choice else {
                continue;
            };
            cost = cost.saturating_add(self.tried[chosen].cost);
            for (class, demand) in &self.tried[chosen].uses {
                if matches!(demand, ChildDemand::Register { .. })
                    && let Some(&input) = self.index.get(class)
                {
                    inputs.push(input);
                }
            }
        }
        Some(cost)
    }

    /// Decide the class for a reader demanding `demand` of its register, and
    /// everything the decision reads. `tiled` forbids the register that
    /// already holds it: a deferred source's view computes itself.
    fn select(&mut self, site: usize, demand: Option<&ChildDemand>, tiled: bool) -> bool {
        match &self.sites[site].state {
            Some(PbqpIselAlternative::Tile { match_id }) => {
                return demand.is_none_or(|demand| self.answers(demand, *match_id));
            }
            Some(PbqpIselAlternative::NotDemanded) if !tiled => {
                return self.sites[site].available;
            }
            Some(_) => return false,
            None => {}
        }
        if self.sites[site].available && !tiled && demand.is_none_or(|demand| self.held(demand)) {
            self.sites[site].state = Some(PbqpIselAlternative::NotDemanded);
            return true;
        }
        // What other classes were given since the class was first priced
        // can only have made its inputs cheaper, so it is decided afresh.
        let Some((match_id, _)) = self.best(site, demand) else {
            return false;
        };
        self.sites[site].state = Some(PbqpIselAlternative::Tile { match_id });
        for at in 0..self.tried[match_id].uses.len() {
            let (class, demand) = self.tried[match_id].uses[at].clone();
            match &demand {
                ChildDemand::Register { .. } => {
                    let child = self.site(class);
                    if !self.select(child, Some(&demand), false) {
                        return false;
                    }
                }
                ChildDemand::Effect => {
                    let child = self.site(class);
                    let site = &mut self.sites[child];
                    let free = !site.demanded || site.available;
                    match &site.state {
                        None => {
                            site.state = Some(if free {
                                PbqpIselAlternative::NotDemanded
                            } else {
                                PbqpIselAlternative::CoveredBy { match_id }
                            });
                        }
                        Some(PbqpIselAlternative::NotDemanded)
                        | Some(PbqpIselAlternative::CoveredBy { .. }) => {}
                        Some(_) => return false,
                    }
                }
                ChildDemand::Immediate | ChildDemand::None => {}
            }
        }
        true
    }
}
