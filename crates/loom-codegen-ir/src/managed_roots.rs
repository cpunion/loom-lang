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
/// collection, while typed Text/Bytes helpers stage their inputs before they
/// can collect.
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
            Repr::Uninhabited
            | Repr::Zst
            | Repr::Scalar(_)
            | Repr::ImmortalText
            | Repr::TaskHandle => {}
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
            let list_allocation = matches!(
                instruction.kind(),
                InstructionKind::ListAppend { .. } | InstructionKind::ListAppendUnique { .. }
            ) || matches!(
                instruction.kind(),
                InstructionKind::ListConstruct { elements } if !elements.is_empty()
            );
            let dyn_allocation = matches!(instruction.kind(), InstructionKind::DynConstruct { .. });
            let collecting = matches!(
                instruction.kind(),
                InstructionKind::TextConcat { .. }
                    | InstructionKind::TextGet { .. }
                    | InstructionKind::TextFromUtf8Units { .. }
                    | InstructionKind::ProcessArgumentAt { .. }
                    | InstructionKind::ProcessEnvironment { .. }
                    | InstructionKind::PathJoin { .. }
                    | InstructionKind::BytesAppend { .. }
                    | InstructionKind::BytesDecodeUtf8 { .. }
                    | InstructionKind::FloatFormat { .. }
                    | InstructionKind::JsonFormat { .. }
                    | InstructionKind::TaskOutcomeTake { .. }
                    | InstructionKind::TextMapInsert { .. }
                    | InstructionKind::TextMapConstructEntries { .. }
                    | InstructionKind::TextMapRemove { .. }
            ) || list_allocation
                || dyn_allocation
                || matches!(
                    instruction.kind(),
                    InstructionKind::DirectCall { callee, .. }
                        if program.function(*callee).is_some_and(|callee| {
                            callee.effects().contains(Effects::MAY_COLLECT)
                        })
                );
            if collecting {
                // Unlike Text helpers and ordinary calls, typed repeated List
                // allocation copies its operands only after the collector can
                // relocate them. They therefore belong to this row even when
                // dead after the instruction.
                if list_allocation
                    || matches!(
                        instruction.kind(),
                        InstructionKind::TextMapInsert { .. }
                            | InstructionKind::TextMapConstructEntries { .. }
                    )
                    || dyn_allocation
                {
                    add_managed(&mut live, instruction.kind().operands(), managed);
                } else if let InstructionKind::TextMapRemove { map, .. } = instruction.kind() {
                    // Removal needs only the immutable source backing after
                    // its conditional allocation. The lookup key has already
                    // been consumed before that safepoint.
                    add_managed(&mut live, [*map], managed);
                }
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
        TerminatorKind::SumSwitch { cases, .. }
        | TerminatorKind::SumBorrowSwitch { cases, .. }
        | TerminatorKind::DynSwitch { cases, .. } => cases.iter().map(|case| case.block).collect(),
        TerminatorKind::SumZipSwitch {
            cases, mismatch, ..
        } => {
            let mut successors = cases.iter().map(|case| case.block).collect::<Vec<_>>();
            successors.push(mismatch.block);
            successors
        }
        TerminatorKind::CheckedIntNegate { normal, fault, .. }
        | TerminatorKind::CheckedIntBinary { normal, fault, .. }
        | TerminatorKind::TaskSleep { normal, fault, .. }
        | TerminatorKind::LogWrite { normal, fault, .. }
        | TerminatorKind::StdoutWrite { normal, fault, .. } => {
            vec![normal.block, fault.block]
        }
        TerminatorKind::Invoke { normal, unwind, .. } => vec![normal.block, unwind.block],
        TerminatorKind::Assert { success, fault, .. } => {
            vec![success.block, fault.block]
        }
        TerminatorKind::AwaitTasks {
            normal,
            fault,
            cancel,
            ..
        } => vec![normal.block, fault.block, cancel.block],
        TerminatorKind::Return(_)
        | TerminatorKind::Fault { .. }
        | TerminatorKind::ResumeFault
        | TerminatorKind::TaskCancelled => Vec::new(),
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
        TerminatorKind::Jump(_)
        | TerminatorKind::Fault { .. }
        | TerminatorKind::ResumeFault
        | TerminatorKind::TaskCancelled => Vec::new(),
        TerminatorKind::Branch { condition, .. } | TerminatorKind::Assert { condition, .. } => {
            vec![*condition]
        }
        TerminatorKind::SumSwitch { scrutinee, .. }
        | TerminatorKind::SumBorrowSwitch { scrutinee, .. }
        | TerminatorKind::DynSwitch { scrutinee, .. }
        | TerminatorKind::Return(scrutinee)
        | TerminatorKind::CheckedIntNegate {
            value: scrutinee, ..
        }
        | TerminatorKind::TaskSleep {
            milliseconds: scrutinee,
            ..
        } => vec![*scrutinee],
        TerminatorKind::SumZipSwitch { left, right, .. }
        | TerminatorKind::CheckedIntBinary { left, right, .. } => vec![*left, *right],
        TerminatorKind::Invoke { arguments, .. } => arguments.to_vec(),
        TerminatorKind::AwaitTasks { tasks, .. } => tasks.to_vec(),
        TerminatorKind::LogWrite {
            level,
            message,
            fields,
            ..
        } => vec![*level, *message, *fields],
        TerminatorKind::StdoutWrite { text, .. } => vec![*text],
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
        TerminatorKind::SumSwitch { cases, .. }
        | TerminatorKind::SumBorrowSwitch { cases, .. }
        | TerminatorKind::DynSwitch { cases, .. } => cases
            .iter()
            .map(|case| (case.block, case.arguments.as_ref()))
            .collect(),
        TerminatorKind::SumZipSwitch {
            cases, mismatch, ..
        } => cases
            .iter()
            .map(|case| (case.block, case.arguments.as_ref()))
            .chain(std::iter::once((
                mismatch.block,
                mismatch.arguments.as_ref(),
            )))
            .collect(),
        TerminatorKind::CheckedIntNegate { normal, fault, .. }
        | TerminatorKind::CheckedIntBinary { normal, fault, .. }
        | TerminatorKind::TaskSleep { normal, fault, .. }
        | TerminatorKind::LogWrite { normal, fault, .. }
        | TerminatorKind::StdoutWrite { normal, fault, .. } => vec![
            (normal.block, normal.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
        ],
        TerminatorKind::Invoke { normal, unwind, .. } => vec![
            (normal.block, normal.arguments.as_ref()),
            (unwind.block, unwind.arguments.as_ref()),
        ],
        TerminatorKind::AwaitTasks {
            normal,
            fault,
            cancel,
            ..
        } => vec![
            (normal.block, normal.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
            (cancel.block, cancel.arguments.as_ref()),
        ],
        TerminatorKind::Assert { success, fault, .. } => vec![
            (success.block, success.arguments.as_ref()),
            (fault.block, fault.arguments.as_ref()),
        ],
        TerminatorKind::Return(_)
        | TerminatorKind::Fault { .. }
        | TerminatorKind::ResumeFault
        | TerminatorKind::TaskCancelled => Vec::new(),
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
    use std::collections::BTreeSet;

    use loom_mir::{FunctionId as MirFunctionId, Type, TypeId};

    use super::{
        MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE, ManagedRootProjection,
        managed_leaf_projections, plan_managed_roots_with_work,
    };
    use crate::{
        BlockTarget, Constant, Effects, InstructionKind, ManagedSafepoint, Origin, ProgramBuilder,
        Signature, TargetLayout, Terminator, TerminatorKind,
    };

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the root-row regression needs two allocation sites and their exact SSA definitions in one minimal checked function"
    )]
    fn list_allocation_roots_dead_operands_but_not_its_undefined_result() {
        let origin = Origin::synthetic(MirFunctionId(0));
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let text = builder
            .add_managed_text_type()
            .expect("register managed Text");
        let list = builder
            .add_managed_list_type(Type::List(Box::new(Type::Text)))
            .expect("register managed List");
        let unit = builder.type_id(&Type::Unit).expect("Unit");
        let root = builder
            .declare_function(
                origin,
                "list_roots",
                Signature::new([], unit),
                Effects::MAY_COLLECT.with_implications(),
            )
            .expect("declare function");
        let (text_value, empty, constructed, appended) = {
            let mut function = builder.function(root).expect("function builder");
            let entry = function.create_block().expect("entry");
            function.set_entry(entry).expect("set entry");
            let text_value = function
                .append_instruction(
                    entry,
                    InstructionKind::TextLiteral { utf8: "x".into() },
                    &[text],
                    origin,
                )
                .expect("Text literal")[0];
            let empty = function
                .append_instruction(
                    entry,
                    InstructionKind::ListConstruct {
                        elements: Box::new([]),
                    },
                    &[list],
                    origin,
                )
                .expect("empty List")[0];
            let construct = function
                .append_instruction(
                    entry,
                    InstructionKind::ListConstruct {
                        elements: Box::new([text_value]),
                    },
                    &[list],
                    origin,
                )
                .expect("List literal");
            let append = function
                .append_instruction(
                    entry,
                    InstructionKind::ListAppend {
                        list: empty,
                        value: text_value,
                    },
                    &[list],
                    origin,
                )
                .expect("List append");
            let result = function
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit],
                    origin,
                )
                .expect("result")[0];
            function
                .terminate(
                    entry,
                    Terminator::new(TerminatorKind::Return(result), origin),
                )
                .expect("return");
            (text_value, empty, construct[0], append[0])
        };
        let program = builder.finish_checked().expect("checked program");
        let function = program
            .as_program()
            .function(root)
            .expect("checked function");
        let instruction_of =
            |value| match function.value(value).expect("checked value").definition() {
                crate::ValueDefinition::InstructionResult { instruction, .. } => instruction,
                crate::ValueDefinition::BlockParameter { .. } => {
                    panic!("expected instruction result")
                }
            };
        let construct_id = instruction_of(constructed);
        let append_id = instruction_of(appended);
        let plan = super::plan_managed_roots(&program, root).expect("managed-root plan");
        let construct_state = plan
            .state(ManagedSafepoint::Instruction(construct_id))
            .expect("construct state");
        let append_state = plan
            .state(ManagedSafepoint::Instruction(append_id))
            .expect("append state");
        let live_values = |state: u64| {
            let state = usize::try_from(state).expect("state index");
            let row = &plan.bitmaps()[state * plan.bitmap_words()..][..plan.bitmap_words()];
            plan.slots()
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    ((row[index / 64] & (1_u64 << (index % 64))) != 0).then_some(slot.value())
                })
                .collect::<BTreeSet<_>>()
        };
        // `empty` is also live through the later append, independently of the
        // construct operand that allocation itself requires.
        assert_eq!(
            live_values(construct_state),
            BTreeSet::from([text_value, empty])
        );
        assert_eq!(
            live_values(append_state),
            BTreeSet::from([text_value, empty])
        );
        assert!(
            !plan
                .slots()
                .iter()
                .any(|slot| { slot.value() == constructed || slot.value() == appended })
        );
    }

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
    fn transparent_carriers_reuse_their_managed_base_projections() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        builder
            .add_managed_text_type()
            .expect("register managed Text");
        let text_wrapper = builder
            .add_transparent_type(Type::Nominal(TypeId(7_000), Vec::new()), &Type::Text)
            .expect("register transparent managed pointer");
        assert_eq!(
            managed_leaf_projections(builder.representations(), text_wrapper)
                .expect("transparent direct-pointer projections")
                .as_ref(),
            [Box::from([])]
        );

        let base = Type::Tuple(vec![Type::Text, Type::Int]);
        builder
            .add_tuple_type(&[Type::Text, Type::Int])
            .expect("register managed product");
        let wrapper = builder
            .add_transparent_type(Type::Nominal(TypeId(7_001), Vec::new()), &base)
            .expect("register transparent managed product");
        let projections = managed_leaf_projections(builder.representations(), wrapper)
            .expect("transparent managed projections");
        assert_eq!(
            projections.as_ref(),
            [Box::from([ManagedRootProjection::ProductField(0)])]
        );
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
