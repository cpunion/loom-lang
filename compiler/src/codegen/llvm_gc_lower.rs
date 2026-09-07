//! Commit GC roots only after source calls have been inlined.
//!
//! Opaque region markers keep addressable snapshots alive during early LLVM
//! optimization. They never reach object code. Each remaining native function
//! gets one root frame; inlined region exits clear their slots, not the frame.

use super::*;
use inkwell::{
    attributes::{Attribute, AttributeLoc},
    values::{BasicValue, CallSiteValue, InstructionOpcode, InstructionValue},
};

const BEGIN: &str = "loom.gc.begin";
const END: &str = "loom.gc.end";

fn marker<'ctx>(context: &'ctx Context, module: &Module<'ctx>, name: &str) -> FunctionValue<'ctx> {
    module.get_function(name).unwrap_or_else(|| {
        let pointer = context.ptr_type(AddressSpace::default());
        let params = if name == BEGIN {
            vec![pointer.into(); 2]
        } else {
            vec![pointer.into()]
        };
        // These calls must be opaque and capturing: allocation can observe the
        // slots before physical root registration has been inserted.
        module.add_function(name, context.void_type().fn_type(&params, false), None)
    })
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
        let mut lifetimes = Vec::new();
        let mut returns = Vec::new();
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
                if name == BEGIN.as_bytes() {
                    let slot = operand(instruction, 0)?;
                    let trace = operand(instruction, 1)?;
                    if let Some(index) = positions.get(&slot) {
                        if slots[*index] != (slot, trace) {
                            return Err("conflicting GC tracers".into());
                        }
                    } else {
                        positions.insert(slot, slots.len());
                        slots.push((slot, trace));
                    }
                    markers.push((instruction, false));
                } else if name == END.as_bytes() {
                    markers.push((instruction, true));
                } else if name.starts_with(b"llvm.lifetime.") {
                    lifetimes.push(instruction);
                }
            }
        }
        if slots.is_empty() {
            continue;
        }
        let entry = function
            .get_first_basic_block()
            .ok_or("missing rooted function entry")?;
        let prologue = context.prepend_basic_block(entry, "gc.entry");
        builder.position_at_end(prologue);
        let mut types = HashMap::new();
        for (slot, _) in &slots {
            let alloca = slot
                .as_instruction_value()
                .ok_or("GC root is not a stack slot")?;
            let ty = alloca
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
            types.insert(*slot, ty);
        }
        for (slot, _) in &slots {
            builder.build_store(*slot, types[slot].const_zero())?;
        }
        let root = context.struct_type(&[pointer.into(), pointer.into()], false);
        let table_type =
            root.array_type(u32::try_from(slots.len()).map_err(|_| "too many GC roots")?);
        let table = builder.build_alloca(table_type, "gc.roots")?;
        let mut entries = table_type.const_zero();
        for (index, (slot, trace)) in slots.iter().enumerate() {
            let item = builder
                .build_insert_value(root.const_zero(), *slot, 0, "gc.address")?
                .into_struct_value();
            let item = builder
                .build_insert_value(item, *trace, 1, "gc.trace")?
                .into_struct_value();
            entries = builder
                .build_insert_value(entries, item, index as u32, "gc.root")?
                .into_array_value();
        }
        builder.build_store(table, entries)?;
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
        for (instruction, end) in markers {
            if end {
                let slot = operand(instruction, 0)?;
                let ty = types.get(&slot).ok_or("unregistered GC region end")?;
                if !ends_at_return(instruction) {
                    builder.position_before(&instruction);
                    builder.build_store(slot, ty.const_zero())?;
                }
            }
            instruction.erase_from_basic_block();
        }
        // LLVM's inliner adds lifetime ends for the old callee's stack storage.
        // The combined root table lives longer, so keep its slots valid. Other
        // lifetimes are conservatively omitted in rooted functions as well;
        // ordinary scalar allocas can still be promoted by the normal pipeline.
        for instruction in lifetimes {
            instruction.erase_from_basic_block();
        }
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
