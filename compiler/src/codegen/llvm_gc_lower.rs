//! Commit GC roots only after source calls have been inlined.
//!
//! Opaque region markers keep addressable snapshots alive during early LLVM
//! optimization. They never reach object code. Each remaining native function
//! gets one root frame. Region entry/exit activates/deactivates descriptors;
//! inactive stack storage need not be initialized or kept alive for the frame.

use super::*;
use inkwell::{
    attributes::{Attribute, AttributeLoc},
    values::{BasicValue, CallSiteValue, InstructionOpcode, InstructionValue},
};
use std::collections::HashSet;

const BEGIN: &str = "loom.gc.begin";
const END: &str = "loom.gc.end";
const DECLARE: &str = "loom.gc.slot";

enum Region {
    Declare,
    Begin,
    End,
}

fn marker<'ctx>(context: &'ctx Context, module: &Module<'ctx>, name: &str) -> FunctionValue<'ctx> {
    module.get_function(name).unwrap_or_else(|| {
        let pointer = context.ptr_type(AddressSpace::default());
        let params = if name == END {
            vec![pointer.into()]
        } else {
            vec![pointer.into(); 2]
        };
        // These calls must be opaque and capturing: allocation can observe the
        // slots before physical root registration has been inserted.
        module.add_function(name, context.void_type().fn_type(&params, false), None)
    })
}

pub(super) fn declare<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    slot: PointerValue<'ctx>,
    trace: PointerValue<'ctx>,
) -> NativeResult<()> {
    // Declaration survives even when optimization removes every capture.
    // It registers storage without making uninitialized contents observable.
    builder.build_call(
        marker(context, module, DECLARE),
        &[slot.into(), trace.into()],
        "",
    )?;
    Ok(())
}

pub(super) fn begin<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    slot: PointerValue<'ctx>,
    trace: PointerValue<'ctx>,
) -> NativeResult<()> {
    builder.build_call(
        marker(context, module, BEGIN),
        &[slot.into(), trace.into()],
        "",
    )?;
    Ok(())
}

pub(super) fn end<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    slot: PointerValue<'ctx>,
) -> NativeResult<()> {
    builder.build_call(marker(context, module, END), &[slot.into()], "")?;
    Ok(())
}

fn operand(instruction: InstructionValue<'_>, index: u32) -> NativeResult<PointerValue<'_>> {
    Ok(instruction
        .get_operand(index)
        .ok_or("missing GC marker operand")?
        .value()
        .ok_or("invalid GC marker operand")?
        .into_pointer_value())
}

fn ends_at_return(mut instruction: InstructionValue<'_>) -> bool {
    while let Some(next) = instruction.get_next_instruction() {
        if next.get_opcode() == InstructionOpcode::Return {
            return true;
        }
        if CallSiteValue::try_from(next)
            .ok()
            .and_then(|call| call.get_called_fn_value())
            .is_none_or(|target| target.get_name().to_bytes() != END.as_bytes())
        {
            return false;
        }
        instruction = next;
    }
    false
}

pub(super) fn lower<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    machine: &TargetMachine,
) -> NativeResult<()> {
    let builder = context.create_builder();
    let size = context.ptr_sized_int_type(&machine.get_target_data(), None);
    let pointer = context.ptr_type(AddressSpace::default());
    for function in module.get_functions() {
        let mut slots = Vec::new();
        let mut positions = HashMap::new();
        let mut markers = Vec::new();
        let mut returns = Vec::new();
        let mut captures = HashSet::new();
        for block in function.get_basic_blocks() {
            for instruction in block.get_instructions() {
                if instruction.get_opcode() == InstructionOpcode::Return {
                    returns.push(instruction);
                }
                let Ok(call) = CallSiteValue::try_from(instruction) else {
                    continue;
                };
                let Some(target) = call.get_called_fn_value() else {
                    continue;
                };
                let name = target.get_name().to_bytes();
                if name == BEGIN.as_bytes() || name == DECLARE.as_bytes() {
                    let slot = operand(instruction, 0)?;
                    let trace = operand(instruction, 1)?;
                    if name == BEGIN.as_bytes() {
                        captures.insert(slot);
                    }
                    if let Some(index) = positions.get(&slot) {
                        if slots[*index] != (slot, trace) {
                            return Err("conflicting GC tracers".into());
                        }
                    } else {
                        positions.insert(slot, slots.len());
                        slots.push((slot, trace));
                    }
                    markers.push((
                        instruction,
                        if name == BEGIN.as_bytes() {
                            Region::Begin
                        } else {
                            Region::Declare
                        },
                    ));
                } else if name == END.as_bytes() {
                    markers.push((instruction, Region::End));
                }
            }
        }
        // Declarations keep storage visible to early optimization, but do not
        // activate it. A slot without any surviving capture cannot be a root.
        let inactive = slots
            .iter()
            .filter(|(slot, _)| !captures.contains(slot))
            .map(|(slot, _)| *slot)
            .collect::<HashSet<_>>();
        let mut active_markers = Vec::with_capacity(markers.len());
        for (instruction, region) in markers {
            if inactive.contains(&operand(instruction, 0)?) {
                instruction.erase_from_basic_block();
            } else {
                active_markers.push((instruction, region));
            }
        }
        slots.retain(|(slot, _)| !inactive.contains(slot));
        positions.clear();
        positions.extend(
            slots
                .iter()
                .enumerate()
                .map(|(index, (slot, _))| (*slot, index)),
        );
        if slots.is_empty() {
            continue;
        }
        let entry = function
            .get_first_basic_block()
            .ok_or("missing rooted function entry")?;
        let prologue = context.prepend_basic_block(entry, "gc.entry");
        builder.position_at_end(prologue);
        for (slot, _) in &slots {
            let alloca = slot
                .as_instruction_value()
                .ok_or("GC root is not a stack slot")?;
            alloca
                .get_allocated_type()
                .map_err(|_| "GC root is not an alloca")?;
            let count = alloca
                .get_operand(0)
                .and_then(|arg| arg.value())
                .and_then(|arg| arg.into_int_value().get_zero_extended_constant());
            if count != Some(1) {
                return Err("GC root must be one static value".into());
            }
            alloca.remove_from_basic_block();
            builder.insert_instruction(&alloca, None);
        }
        let root = context.struct_type(&[pointer.into(), pointer.into()], false);
        let table_type =
            root.array_type(u32::try_from(slots.len()).map_err(|_| "too many GC roots")?);
        let table = builder.build_alloca(table_type, "gc.roots")?;
        // All tracers remain valid function pointers, including inactive entries.
        // This is one literal constant, not a quadratic insertvalue chain.
        let entries = slots
            .iter()
            .map(|(_, trace)| {
                root.const_named_struct(&[pointer.const_null().into(), (*trace).into()])
            })
            .collect::<Vec<_>>();
        builder.build_store(table, root.const_array(&entries))?;
        let frame_type = context.struct_type(&[pointer.into(), pointer.into(), size.into()], false);
        let frame = builder.build_alloca(frame_type, "gc.frame")?;
        let enter = gc::runtime_function(
            context,
            module,
            "roots_enter",
            None,
            &[pointer.into(), pointer.into(), size.into()],
        );
        builder.build_call(
            enter,
            &[
                frame.into(),
                table.into(),
                size.const_int(slots.len() as u64, false).into(),
            ],
            "",
        )?;
        builder.build_unconditional_branch(entry)?;
        let mut active_block = None;
        let mut active = HashMap::new();
        for (instruction, region) in active_markers {
            let end = matches!(region, Region::End);
            if !matches!(region, Region::Declare) && (!end || !ends_at_return(instruction)) {
                let slot = operand(instruction, 0)?;
                let index = positions.get(&slot).ok_or("unregistered GC region")?;
                let block = instruction.get_parent().ok_or("missing GC region block")?;
                if active_block != Some(block) {
                    active.clear();
                    active_block = Some(block);
                }
                // Collection changes rooted payloads, never descriptor addresses.
                // Only reuse knowledge within this straight-line basic block.
                if active.insert(slot, !end) != Some(!end) {
                    builder.position_before(&instruction);
                    // SAFETY: each region refers to one entry in this fixed table.
                    #[allow(unsafe_code)]
                    let item = unsafe {
                        builder.build_in_bounds_gep(
                            table_type,
                            table,
                            &[size.const_zero(), size.const_int(*index as u64, false)],
                            "gc.root",
                        )?
                    };
                    let address = builder.build_struct_gep(root, item, 0, "gc.address")?;
                    builder.build_store(address, if end { pointer.const_null() } else { slot })?;
                }
            }
            instruction.erase_from_basic_block();
        }
        // Inlined lifetime ends are now safe: the corresponding descriptor is
        // inactive before storage expires, even though the combined frame stays.
        let leave = gc::runtime_function(context, module, "roots_leave", None, &[pointer.into()]);
        for instruction in returns {
            builder.position_before(&instruction);
            builder.build_call(leave, &[frame.into()], "")?;
        }
        // Later scalar optimization remains enabled, but cannot splice an
        // already-finalized frame back into its caller. Unrooted helpers retain
        // the normal inlining and devirtualization opportunities.
        function.add_attribute(
            AttributeLoc::Function,
            context.create_enum_attribute(Attribute::get_named_enum_kind_id("noinline"), 0),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::memory_buffer::MemoryBuffer;

    #[test]
    fn cold_regions_activate_after_initialization_and_expire_before_lifetime_end() {
        let context = Context::create();
        let input = MemoryBuffer::create_from_memory_range_copy(
            br#"
                declare void @trace(ptr)
                declare void @allocate()
                declare void @loom.gc.begin(ptr, ptr)
                declare void @loom.gc.end(ptr)
                declare void @llvm.lifetime.start.p0(ptr)
                declare void @llvm.lifetime.end.p0(ptr)

                define void @guarded(i1 %condition) {
                entry:
                    %slot = alloca ptr
                    br i1 %condition, label %slow, label %fast
                slow:
                    call void @llvm.lifetime.start.p0(ptr %slot)
                    store ptr null, ptr %slot
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    call void @allocate()
                    call void @loom.gc.end(ptr %slot)
                    call void @llvm.lifetime.end.p0(ptr %slot)
                    br label %done
                fast:
                    br label %done
                done:
                    call void @allocate()
                    ret void
                }
            "#,
            "gc.regions",
        );
        let module = context.create_module_from_ir(input).unwrap();
        let target = NativeTarget::new(Optimization::O0).unwrap();
        lower(&context, &module, &target.machine).unwrap();
        module.verify().unwrap();
        let ir = module.print_to_string().to_string();
        let prologue = ir.split("define void @guarded").nth(1).unwrap();
        let prologue = prologue.split("\nentry:").next().unwrap();
        assert!(prologue.contains("{ ptr null, ptr @trace }"));
        assert!(!prologue.contains("store ptr null, ptr"));
        let slow = ir.split("\nslow:").nth(1).unwrap();
        let slow = slow.split("\nfast:").next().unwrap();
        let initialized = slow.find("store ptr null").unwrap();
        let activated = slow.find("store ptr %").unwrap();
        let allocation = slow.find("call void @allocate()").unwrap();
        let deactivated = slow.rfind("store ptr null").unwrap();
        let expired = slow.find("call void @llvm.lifetime.end.p0").unwrap();
        assert!(initialized < activated && activated < allocation);
        assert!(allocation < deactivated && deactivated < expired);
        assert!(!ir.contains("call void @loom.gc."));
        assert_eq!(ir.matches("call void @loom_rt_roots_enter(").count(), 1);
        assert_eq!(ir.matches("call void @loom_rt_roots_leave(").count(), 1);
    }

    #[test]
    fn eliminated_captures_do_not_require_a_physical_root_frame() {
        let context = Context::create();
        let input = MemoryBuffer::create_from_memory_range_copy(
            br#"
                declare void @trace(ptr)
                declare void @allocate()
                declare void @loom.gc.slot(ptr, ptr)
                declare void @loom.gc.begin(ptr, ptr)
                declare void @loom.gc.end(ptr)

                define void @unused() {
                entry:
                    %slot = alloca ptr
                    call void @loom.gc.slot(ptr %slot, ptr @trace)
                    br i1 false, label %capture, label %done
                capture:
                    store ptr null, ptr %slot
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    br label %done
                done:
                    call void @allocate()
                    call void @loom.gc.end(ptr %slot)
                    call void @allocate()
                    ret void
                }
            "#,
            "gc.unused",
        );
        let module = context.create_module_from_ir(input).unwrap();
        let target = NativeTarget::new(Optimization::O2).unwrap();
        module
            .run_passes(
                "function(simplifycfg)",
                &target.machine,
                PassBuilderOptions::create(),
            )
            .unwrap();
        assert!(
            !module
                .print_to_string()
                .to_string()
                .contains("call void @loom.gc.begin")
        );
        lower(&context, &module, &target.machine).unwrap();
        module.verify().unwrap();
        let ir = module.print_to_string().to_string();
        assert!(!ir.contains("call void @loom.gc."));
        assert!(!ir.contains("store ptr %"));
        assert!(!ir.contains("loom_rt_roots_enter"));
        assert!(!ir.contains("loom_rt_roots_leave"));
    }

    #[test]
    fn pruning_inactive_slots_preserves_the_remaining_descriptor_indices() {
        let context = Context::create();
        let input = MemoryBuffer::create_from_memory_range_copy(
            br#"
                declare void @unused_trace(ptr)
                declare void @live_trace(ptr)
                declare void @allocate()
                declare void @loom.gc.slot(ptr, ptr)
                declare void @loom.gc.begin(ptr, ptr)
                declare void @loom.gc.end(ptr)

                define void @mixed() {
                entry:
                    %unused = alloca ptr
                    %live = alloca ptr
                    call void @loom.gc.slot(ptr %unused, ptr @unused_trace)
                    store ptr null, ptr %live
                    call void @loom.gc.begin(ptr %live, ptr @live_trace)
                    call void @allocate()
                    call void @loom.gc.end(ptr %unused)
                    call void @allocate()
                    call void @loom.gc.end(ptr %live)
                    ret void
                }
            "#,
            "gc.mixed",
        );
        let module = context.create_module_from_ir(input).unwrap();
        let target = NativeTarget::new(Optimization::O0).unwrap();
        lower(&context, &module, &target.machine).unwrap();
        module.verify().unwrap();
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("alloca [1 x { ptr, ptr }]"));
        assert!(ir.contains("{ ptr null, ptr @live_trace }"));
        assert!(!ir.contains("{ ptr null, ptr @unused_trace }"));
        assert!(!ir.contains("call void @loom.gc."));
        assert_eq!(ir.matches("call void @loom_rt_roots_enter(").count(), 1);
        assert_eq!(ir.matches("call void @loom_rt_roots_leave(").count(), 1);
    }

    #[test]
    fn descriptor_writes_reuse_only_known_state_within_one_basic_block() {
        let context = Context::create();
        let input = MemoryBuffer::create_from_memory_range_copy(
            br#"
                declare void @trace(ptr)
                declare void @allocate()
                declare void @loom.gc.begin(ptr, ptr)
                declare void @loom.gc.end(ptr)

                define void @straight() {
                entry:
                    %slot = alloca ptr
                    store ptr null, ptr %slot
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    call void @allocate()
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    call void @allocate()
                    call void @loom.gc.end(ptr %slot)
                    call void @allocate()
                    call void @loom.gc.end(ptr %slot)
                    call void @allocate()
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    call void @allocate()
                    ret void
                }

                define void @joined(i1 %condition) {
                entry:
                    %slot = alloca ptr
                    store ptr null, ptr %slot
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    br i1 %condition, label %expire, label %live
                expire:
                    call void @loom.gc.end(ptr %slot)
                    br label %done
                live:
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    br label %done
                done:
                    call void @loom.gc.begin(ptr %slot, ptr @trace)
                    call void @allocate()
                    ret void
                }
            "#,
            "gc.repeated",
        );
        let module = context.create_module_from_ir(input).unwrap();
        let target = NativeTarget::new(Optimization::O0).unwrap();
        lower(&context, &module, &target.machine).unwrap();
        module.verify().unwrap();
        let ir = module.print_to_string().to_string();
        let straight = ir.split("define void @straight").nth(1).unwrap();
        let straight = straight.split("define void @joined").next().unwrap();
        assert_eq!(
            straight.matches("store ptr %slot, ptr %gc.address").count(),
            2
        );
        assert_eq!(
            straight.matches("store ptr null, ptr %gc.address").count(),
            1
        );
        let joined = ir.split("define void @joined").nth(1).unwrap();
        assert_eq!(
            joined.matches("store ptr %slot, ptr %gc.address").count(),
            3
        );
        assert_eq!(joined.matches("store ptr null, ptr %gc.address").count(), 1);
        assert_eq!(ir.matches("call void @allocate()").count(), 6);
        assert!(!ir.contains("call void @loom.gc."));
    }
}
