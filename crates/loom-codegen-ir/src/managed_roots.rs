use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    BlockId, CheckedProgram, Effects, Function, InstanceId, InstructionId, InstructionKind, Repr,
    RepresentationPlan, TerminatorKind, ValueId, ValueTypeId,
};

/// A moving-GC safepoint in one checked LCIR function.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ManagedSafepoint {
    Instruction(InstructionId),
    Terminator(BlockId),
}

/// One typed step from an LCIR aggregate to an exact managed leaf.
///
/// Sum steps name both the closed variant and its payload field. They are
/// candidate projections: publication additionally tests every enclosing tag
/// and writes null for an inactive candidate.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ManagedRootProjection {
    ProductField(u32),
    SumVariantField { variant: u32, field: u32 },
}

/// Maximum candidate managed leaves catalogued for one SSA value.
///
/// Direct aggregate validation expands repeated children by occurrence and
/// applies the same structural bound, so this is both an explicit root-planner
/// resource boundary and a defense against independently forged plans.
pub const MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE: usize =
    crate::repr::DIRECT_PRODUCT_MAX_STRUCTURAL_NODES;

/// One exact candidate managed leaf of an LCIR SSA value.
///
/// `projection` is empty for a direct managed pointer. Slots sort first by
/// dense `ValueId` and then lexicographically by their typed projection, which
/// makes root metadata independent from hash or traversal order.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ManagedRootSlot {
    value: ValueId,
    projection: Box<[ManagedRootProjection]>,
}

impl ManagedRootSlot {
    #[must_use]
    pub const fn value(&self) -> ValueId {
        self.value
    }

    #[must_use]
    pub const fn projection(&self) -> &[ManagedRootProjection] {
        &self.projection
    }
}

/// Exact direct-pointer shadow-frame plan derived from checked SSA.
///
/// Bitmap row zero is always empty; every remaining row is a deduplicated
/// live-after set of managed leaf slots for one collecting operation. The
/// operation result is not defined at its safepoint and is therefore excluded
/// from that row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedRootPlan {
    slots: Box<[ManagedRootSlot]>,
    bitmap_words: usize,
    bitmaps: Box<[u64]>,
    states: BTreeMap<ManagedSafepoint, u64>,
}

impl ManagedRootPlan {
    #[must_use]
    pub const fn slots(&self) -> &[ManagedRootSlot] {
        &self.slots
    }

    #[must_use]
    pub const fn bitmap_words(&self) -> usize {
        self.bitmap_words
    }

    #[must_use]
    pub const fn bitmaps(&self) -> &[u64] {
        &self.bitmaps
    }

    #[must_use]
    pub fn state_count(&self) -> usize {
        if self.slots.is_empty() {
            0
        } else {
            self.bitmaps.len() / self.bitmap_words
        }
    }

    #[must_use]
    pub fn state(&self, site: ManagedSafepoint) -> Option<u64> {
        self.states.get(&site).copied()
    }
}

/// Plans exact live managed SSA values for one checked function.
///
/// A collecting call's ordinary arguments are deliberately added only after
/// recording its live-after set. They need no caller root unless independently
/// live after the call: ordinary callees publish parameters before their first
/// collection, while the typed Text helper stages both inputs before it can
/// collect.
#[must_use]
pub fn plan_managed_roots(
    program: &CheckedProgram,
    function: InstanceId,
) -> Option<ManagedRootPlan> {
    plan_managed_roots_with_work(program, function).map(|(plan, _)| plan)
}

fn plan_managed_roots_with_work(
    program: &CheckedProgram,
    function: InstanceId,
) -> Option<(ManagedRootPlan, usize)> {
    let program = program.as_program();
    let function = program.function(function)?;
    let projections = managed_value_projections(program.representations(), function)?;
    let managed = (0..function.values().len())
        .map(|index| {
            projections
                .for_value_index(index)
                .is_some_and(|projections| !projections.is_empty())
        })
        .collect::<Vec<_>>();
    let (live_out, block_evaluations) = analyze_live_out(function, &managed)?;
    let sites = collect_safepoint_values(program, function, &managed, &live_out)?;
    build_root_plan(sites, &projections, block_evaluations)
}

struct ManagedProjectionCatalog {
    value_types: Box<[ValueTypeId]>,
    by_type: BTreeMap<ValueTypeId, Box<[Box<[ManagedRootProjection]>]>>,
}

impl ManagedProjectionCatalog {
    fn for_value_index(&self, index: usize) -> Option<&[Box<[ManagedRootProjection]>]> {
        self.value_types
            .get(index)
            .and_then(|ty| self.by_type.get(ty))
            .map(AsRef::as_ref)
    }
}

fn managed_value_projections(
    representations: &RepresentationPlan,
    function: &Function,
) -> Option<ManagedProjectionCatalog> {
    let value_types = function
        .values()
        .iter()
        .map(crate::Value::ty)
        .collect::<Box<[_]>>();
    let by_type = value_types
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|ty| managed_leaf_projections(representations, ty).map(|paths| (ty, paths)))
        .collect::<Option<BTreeMap<_, _>>>()?;
    Some(ManagedProjectionCatalog {
        value_types,
        by_type,
    })
}

fn managed_leaf_projections(
    representations: &RepresentationPlan,
    root: ValueTypeId,
) -> Option<Box<[Box<[ManagedRootProjection]>]>> {
    let mut projections = Vec::new();
    let mut pending = vec![(root, Vec::new())];
    while let Some((value, path)) = pending.pop() {
        if path.len() > crate::repr::DIRECT_PRODUCT_MAX_NESTING_DEPTH
            || pending
                .len()
                .checked_add(projections.len())?
                .checked_add(1)?
                > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE
        {
            return None;
        }
        let value = representations.value_type(value)?;
        match representations.repr(value.repr())? {
            Repr::ManagedPointer => {
                projections.push(path.into_boxed_slice());
                if projections.len() > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE {
                    return None;
                }
            }
            Repr::Product(product) => {
                for (index, field) in representations
                    .product(*product)?
                    .fields()
                    .iter()
                    .copied()
                    .enumerate()
                    .rev()
                {
                    let index = u32::try_from(index).ok()?;
                    let mut field_path = path.clone();
                    field_path.push(ManagedRootProjection::ProductField(index));
                    pending.push((field, field_path));
                }
            }
            Repr::Sum(sum) => {
                for (variant, payload) in representations
                    .sum(*sum)?
                    .variants()
                    .iter()
                    .enumerate()
                    .rev()
                {
                    let variant = u32::try_from(variant).ok()?;
                    for (field, field_type) in payload.fields().iter().copied().enumerate().rev() {
                        let field = u32::try_from(field).ok()?;
                        let mut field_path = path.clone();
                        field_path.push(ManagedRootProjection::SumVariantField { variant, field });
                        pending.push((field_type, field_path));
                    }
                }
            }
            Repr::Uninhabited | Repr::Zst | Repr::Scalar(_) | Repr::ImmortalText => {}
        }
    }
    Some(projections.into_boxed_slice())
}

fn analyze_live_out(
    function: &Function,
    managed: &[bool],
) -> Option<(Vec<BTreeSet<ValueId>>, usize)> {
    let mut live_in = vec![BTreeSet::new(); function.blocks().len()];
    let mut live_out = vec![BTreeSet::new(); function.blocks().len()];
    let mut live_params = vec![BTreeSet::new(); function.blocks().len()];
    let predecessors = predecessors(function)?;
    let mut pending = function
        .blocks()
        .iter()
        .rev()
        .map(crate::Block::id)
        .collect::<VecDeque<_>>();
    let mut queued = vec![true; function.blocks().len()];
    let mut block_evaluations = 0_usize;
    while let Some(block_id) = pending.pop_front() {
        queued[block_id.index()] = false;
        block_evaluations = block_evaluations.checked_add(1)?;
        let block = function.block(block_id)?;
        let out = edge_live_values(
            function,
            block.terminator()?.kind(),
            &live_in,
            &live_params,
            managed,
        )?;
        let mut input = out.clone();
        add_terminator_local_uses(&mut input, block.terminator()?, managed);
        for instruction in block.instructions().iter().rev() {
            let instruction = function.instruction(*instruction)?;
            for result in instruction.results() {
                input.remove(result);
            }
            add_managed(&mut input, instruction.kind().operands(), managed);
        }
        let parameters = input
            .iter()
            .copied()
            .filter(|value| block.params().contains(value))
            .collect::<BTreeSet<_>>();
        for parameter in block.params() {
            input.remove(parameter);
        }
        let changed = live_out[block_id.index()] != out
            || live_in[block_id.index()] != input
            || live_params[block_id.index()] != parameters;
        if changed {
            live_out[block_id.index()] = out;
            live_in[block_id.index()] = input;
            live_params[block_id.index()] = parameters;
            for predecessor in predecessors.get(block_id.index())? {
                if !queued[predecessor.index()] {
                    queued[predecessor.index()] = true;
                    pending.push_back(*predecessor);
                }
            }
        }
    }
    Some((live_out, block_evaluations))
}

fn collect_safepoint_values(
    program: &crate::Program,
    function: &Function,
    managed: &[bool],
    live_out: &[BTreeSet<ValueId>],
) -> Option<BTreeMap<ManagedSafepoint, BTreeSet<ValueId>>> {
    let mut site_values = BTreeMap::<ManagedSafepoint, BTreeSet<ValueId>>::new();
    for block in function.blocks() {
        let terminator = block.terminator()?;
        let mut live = live_out[block.id().index()].clone();
        match terminator.kind() {
            TerminatorKind::Invoke {
                callee, arguments, ..
            } if program
                .function(*callee)
                .is_some_and(|callee| callee.effects().contains(Effects::MAY_COLLECT)) =>
            {
                add_managed(&mut live, terminator.writebacks().iter().copied(), managed);
                site_values.insert(ManagedSafepoint::Terminator(block.id()), live.clone());
                add_managed(&mut live, arguments.iter().copied(), managed);
            }
            _ => add_terminator_local_uses(&mut live, terminator, managed),
        }
        for instruction_id in block.instructions().iter().rev() {
            let instruction = function.instruction(*instruction_id)?;
            for result in instruction.results() {
                live.remove(result);
            }
            let collecting = matches!(
                instruction.kind(),
                InstructionKind::TextConcat { .. } | InstructionKind::TextGet { .. }
            ) || matches!(
                instruction.kind(),
                InstructionKind::DirectCall { callee, .. }
                    if program.function(*callee).is_some_and(|callee| {
                        callee.effects().contains(Effects::MAY_COLLECT)
                    })
            );
            if collecting {
                site_values.insert(
                    ManagedSafepoint::Instruction(instruction.id()),
                    live.clone(),
                );
            }
            add_managed(&mut live, instruction.kind().operands(), managed);
        }
    }
    Some(site_values)
}

fn build_root_plan(
    site_values: BTreeMap<ManagedSafepoint, BTreeSet<ValueId>>,
    projections: &ManagedProjectionCatalog,
    block_evaluations: usize,
) -> Option<(ManagedRootPlan, usize)> {
    let slots = site_values
        .values()
        .flat_map(BTreeSet::iter)
        .flat_map(|value| {
            projections
                .for_value_index(value.index())
                .into_iter()
                .flat_map(|paths| {
                    paths.iter().cloned().map(|projection| ManagedRootSlot {
                        value: *value,
                        projection,
                    })
                })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if slots.is_empty() {
        return Some((
            ManagedRootPlan {
                slots: Box::new([]),
                bitmap_words: 0,
                bitmaps: Box::new([]),
                states: BTreeMap::new(),
            },
            block_evaluations,
        ));
    }
    let bitmap_words = slots.len().div_ceil(u64::BITS as usize);
    let slot_indices = slots
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, slot)| (slot, index))
        .collect::<BTreeMap<_, _>>();
    let empty = vec![0_u64; bitmap_words];
    let mut rows = vec![empty.clone()];
    let mut row_indices = BTreeMap::from([(empty, 0_u64)]);
    let mut states = BTreeMap::new();
    for (site, values) in site_values {
        let mut row = vec![0_u64; bitmap_words];
        for value in values {
            for projection in projections.for_value_index(value.index())? {
                let key = ManagedRootSlot {
                    value,
                    projection: projection.clone(),
                };
                let slot = slot_indices[&key];
                row[slot / u64::BITS as usize] |= 1_u64 << (slot % u64::BITS as usize);
            }
        }
        let state = if let Some(state) = row_indices.get(&row) {
            *state
        } else {
            let state = u64::try_from(rows.len()).ok()?;
            row_indices.insert(row.clone(), state);
            rows.push(row);
            state
        };
        states.insert(site, state);
    }
    Some((
        ManagedRootPlan {
            slots: slots.into_boxed_slice(),
            bitmap_words,
            bitmaps: rows.into_iter().flatten().collect(),
            states,
        },
        block_evaluations,
    ))
}

fn predecessors(function: &Function) -> Option<Vec<Vec<BlockId>>> {
    let mut predecessors = vec![Vec::new(); function.blocks().len()];
    for block in function.blocks() {
        for successor in successors(block.terminator()?.kind()) {
            predecessors.get_mut(successor.index())?.push(block.id());
        }
    }
    Some(predecessors)
}

fn successors(kind: &TerminatorKind) -> Vec<BlockId> {
    match kind {
        TerminatorKind::Jump(target) => vec![target.block],
        TerminatorKind::Branch {
            then_target,
            else_target,
            ..
        } => vec![then_target.block, else_target.block],
        TerminatorKind::SumSwitch { cases, .. } => cases.iter().map(|case| case.block).collect(),
        TerminatorKind::CheckedIntNegate { normal, fault, .. }
        | TerminatorKind::CheckedIntBinary { normal, fault, .. }
        | TerminatorKind::ResourceClose { normal, fault, .. } => {
            vec![normal.block, fault.block]
        }
        TerminatorKind::Invoke { normal, unwind, .. } => vec![normal.block, unwind.block],
        TerminatorKind::Assert { success, fault, .. } => vec![success.block, fault.block],
        TerminatorKind::Return(_) | TerminatorKind::Fault { .. } | TerminatorKind::ResumeFault => {
            Vec::new()
        }
    }
}

fn add_managed(
    live: &mut BTreeSet<ValueId>,
    values: impl IntoIterator<Item = ValueId>,
    managed: &[bool],
) {
    live.extend(
        values
            .into_iter()
            .filter(|value| managed.get(value.index()).copied().unwrap_or(false)),
    );
}

fn add_terminator_local_uses(
    live: &mut BTreeSet<ValueId>,
    terminator: &crate::Terminator,
    managed: &[bool],
) {
    let values = match terminator.kind() {
        TerminatorKind::Jump(_) | TerminatorKind::Fault { .. } | TerminatorKind::ResumeFault => {
            Vec::new()
        }
        TerminatorKind::Branch { condition, .. } | TerminatorKind::Assert { condition, .. } => {
            vec![*condition]
        }
        TerminatorKind::SumSwitch { scrutinee, .. }
        | TerminatorKind::Return(scrutinee)
        | TerminatorKind::CheckedIntNegate {
            value: scrutinee, ..
        } => vec![*scrutinee],
        TerminatorKind::CheckedIntBinary { left, right, .. } => vec![*left, *right],
        TerminatorKind::Invoke { arguments, .. } => arguments.to_vec(),
        TerminatorKind::ResourceClose { resource, .. } => vec![*resource],
    };
    add_managed(live, values, managed);
    add_managed(live, terminator.writebacks().iter().copied(), managed);
}

fn edge_live_values(
    function: &Function,
    kind: &TerminatorKind,
    live_in: &[BTreeSet<ValueId>],
    live_params: &[BTreeSet<ValueId>],
    managed: &[bool],
) -> Option<BTreeSet<ValueId>> {
    let mut live = BTreeSet::new();
    let edges = match kind {
        TerminatorKind::Jump(target) => vec![(target.block, target.arguments.as_ref())],
        TerminatorKind::Branch {
            then_target,
            else_target,
            ..
        } => vec![
            (then_target.block, then_target.arguments.as_ref()),
            (else_target.block, else_target.arguments.as_ref()),
        ],
        TerminatorKind::SumSwitch { cases, .. } => cases
            .iter()
            .map(|case| (case.block, case.arguments.as_ref()))
            .collect(),
        TerminatorKind::CheckedIntNegate { normal, fault, .. }
        | TerminatorKind::CheckedIntBinary { normal, fault, .. }
        | TerminatorKind::ResourceClose { normal, fault, .. } => vec![
            (normal.block, normal.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
        ],
        TerminatorKind::Invoke { normal, unwind, .. } => vec![
            (normal.block, normal.arguments.as_ref()),
            (unwind.block, unwind.arguments.as_ref()),
        ],
        TerminatorKind::Assert { success, fault, .. } => vec![
            (success.block, success.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
        ],
        TerminatorKind::Return(_) | TerminatorKind::Fault { .. } | TerminatorKind::ResumeFault => {
            Vec::new()
        }
    };
    for (destination, arguments) in edges {
        add_edge_live(
            &mut live,
            function,
            destination,
            arguments,
            live_in,
            live_params,
            managed,
        )?;
    }
    Some(live)
}

fn add_edge_live(
    live: &mut BTreeSet<ValueId>,
    function: &Function,
    destination: BlockId,
    arguments: &[ValueId],
    live_in: &[BTreeSet<ValueId>],
    live_params: &[BTreeSet<ValueId>],
    managed: &[bool],
) -> Option<()> {
    let block = function.block(destination)?;
    live.extend(live_in.get(destination.index())?.iter().copied());
    let offset = block.params().len().checked_sub(arguments.len())?;
    for (parameter, argument) in block.params()[offset..].iter().zip(arguments) {
        if live_params.get(destination.index())?.contains(parameter) {
            add_managed(live, [*argument], managed);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use loom_mir::{FunctionId as MirFunctionId, Type};

    use super::{
        MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE, managed_leaf_projections,
        plan_managed_roots_with_work,
    };
    use crate::{
        BlockTarget, Constant, Effects, InstructionKind, Origin, ProgramBuilder, Signature,
        TargetLayout, Terminator, TerminatorKind,
    };

    #[test]
    fn candidate_catalog_accepts_its_exact_limit_and_rejects_one_more() {
        for (width, accepted) in [
            (MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE, true),
            (MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE + 1, false),
        ] {
            let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
            builder
                .add_managed_text_type()
                .expect("register managed Text");
            let elements = vec![Type::Text; width];
            let product = builder
                .add_tuple_type(&elements)
                .expect("unchecked wide product representation");
            let projections = managed_leaf_projections(builder.representations(), product);
            assert_eq!(projections.is_some(), accepted, "candidate width {width}");
            if let Some(projections) = projections {
                assert_eq!(projections.len(), width);
            }
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one large cyclic LCIR fixture keeps the work bound directly reviewable"
    )]
    fn predecessor_worklist_converges_linearly_on_a_large_loop() {
        const LOOP_BLOCKS: usize = 512;

        let origin = Origin::synthetic(MirFunctionId(0));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let text = builder
            .add_managed_text_type()
            .expect("register managed Text");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let boolean = builder.type_id(&Type::Bool).expect("Bool");
        let integer = builder.type_id(&Type::Int).expect("Int");
        let root = builder
            .declare_function(
                origin,
                "looping",
                Signature::new([], unit),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("declare function");
        let live = {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            let loop_blocks = (0..LOOP_BLOCKS)
                .map(|_| function.create_block().expect("loop block"))
                .collect::<Vec<_>>();
            let exit = function.create_block().expect("exit");
            function.set_entry(entry).expect("set entry");
            let live = function
                .append_instruction(
                    entry,
                    InstructionKind::TextLiteral {
                        utf8: "live".into(),
                    },
                    &[text],
                    origin,
                )
                .expect("live Text")[0];
            let condition = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Bool(true)),
                    &[boolean],
                    origin,
                )
                .expect("loop condition")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Jump(BlockTarget::new(loop_blocks[0], [])),
                        origin,
                    ),
                )
                .expect("enter loop");
            function
                .append_instruction(
                    loop_blocks[0],
                    InstructionKind::TextConcat {
                        left: live,
                        right: live,
                    },
                    &[text],
                    origin,
                )
                .expect("collecting operation");
            for window in loop_blocks.windows(2) {
                function
                    .terminate(
                        window[0],
                        Terminator::new(
                            TerminatorKind::Jump(BlockTarget::new(window[1], [])),
                            origin,
                        ),
                    )
                    .expect("loop chain");
            }
            function
                .terminate(
                    *loop_blocks.last().expect("loop tail"),
                    Terminator::new(
                        TerminatorKind::Branch {
                            condition,
                            then_target: BlockTarget::new(loop_blocks[0], []),
                            else_target: BlockTarget::new(exit, []),
                        },
                        origin,
                    ),
                )
                .expect("loop backedge");
            function
                .append_instruction(
                    exit,
                    InstructionKind::TextLength { text: live },
                    &[integer],
                    origin,
                )
                .expect("post-loop use");
            let result = function
                .append_instruction(
                    exit,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("result")[0];
            function
                .terminate(
                    exit,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
            live
        };
        let program = builder.finish_checked().expect("checked program");
        let block_count = LOOP_BLOCKS + 2;
        let (plan, evaluations) =
            plan_managed_roots_with_work(&program, root).expect("managed-root plan");

        assert_eq!(plan.slots().len(), 1);
        assert_eq!(plan.slots()[0].value(), live);
        assert!(plan.slots()[0].projection().is_empty());
        assert!(
            evaluations <= block_count + 4,
            "predecessor worklist reevaluated {evaluations} blocks for {block_count} CFG blocks"
        );
    }
}
