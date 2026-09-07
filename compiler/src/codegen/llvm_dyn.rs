//! Explicit witness tables and GC-owned snapshots for erased receivers.

use super::*;
use inkwell::types::{FunctionType, StructType};

fn method_type<'ctx>(
    context: &'ctx Context,
    program: &checked::Program,
    method: &checked::Signature,
) -> NativeResult<FunctionType<'ctx>> {
    let mut params = vec![context.ptr_type(AddressSpace::default()).into()];
    for ty in &method.params {
        params.push(native_type(context, program, *ty)?.into());
    }
    Ok(if method.result == Type::Unit {
        context.void_type().fn_type(&params, false)
    } else {
        native_type(context, program, method.result)?.fn_type(&params, false)
    })
}

fn table_type<'ctx>(context: &'ctx Context, methods: usize) -> StructType<'ctx> {
    context.struct_type(
        &vec![context.ptr_type(AddressSpace::default()).into(); methods],
        false,
    )
}

pub(super) fn emit_witnesses<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &checked::Program,
    functions: &[Option<FunctionValue<'ctx>>],
    live: &BTreeSet<usize>,
    used: &BTreeSet<(Type, usize)>,
) -> NativeResult<Vec<Option<PointerValue<'ctx>>>> {
    let pointer = context.ptr_type(AddressSpace::default());
    let builder = context.create_builder();
    let mut tables = vec![None; program.witnesses.len()];
    for &id in live {
        let witness = &program.witnesses[id];
        let interface = &program.interfaces[witness.interface];
        let mut slots = Vec::new();
        for (slot, target) in witness.methods.iter().enumerate() {
            if !used.contains(&(Type::Dyn(witness.interface), slot)) {
                slots.push(pointer.const_null().into());
                continue;
            }
            let Some(target) = target else {
                slots.push(pointer.const_null().into());
                continue;
            };
            let signature = &interface.methods[slot];
            let thunk = module.add_function(
                &format!("loom.witness.{id}.method.{slot}"),
                method_type(context, program, signature)?,
                Some(Linkage::Internal),
            );
            builder.position_at_end(context.append_basic_block(thunk, "entry"));
            let data = thunk
                .get_first_param()
                .ok_or("missing dyn receiver")?
                .into_pointer_value();
            let concrete = builder.build_load(
                native_type(context, program, witness.concrete)?,
                data,
                "dyn.self",
            )?;
            let mut arguments = vec![concrete.into()];
            for argument in thunk.get_param_iter().skip(1) {
                arguments.push(argument.into());
            }
            // The caller roots the box, and the ordinary impl function roots
            // its value parameters before any allocating operation.
            let call = builder.build_call(
                functions[*target].ok_or("missing checked witness method")?,
                &arguments,
                if signature.result == Type::Unit {
                    ""
                } else {
                    "dyn.result"
                },
            )?;
            if let Some(value) = call.try_as_basic_value().basic() {
                builder.build_return(Some(&value))?;
            } else {
                builder.build_return(None)?;
            }
            slots.push(thunk.as_global_value().as_pointer_value().into());
        }
        let value = context.const_struct(&slots, false);
        let table = module.add_global(value.get_type(), None, &format!("loom.witness.{id}"));
        table.set_initializer(&value);
        table.set_constant(true);
        table.set_linkage(Linkage::Private);
        tables[id] = Some(table.as_pointer_value());
    }
    Ok(tables)
}

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    pub(super) fn dyn_box(
        &mut self,
        witness: usize,
        source: &checked::Expr,
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let Some(value) = self.expr(source)? else {
            return Ok(None);
        };
        let shape = &self.program.witnesses[witness];
        let native = native_type(self.context, self.program, shape.concrete)?;
        let pointer = self.context.ptr_type(AddressSpace::default());
        let trace = if gc::managed(self.program, shape.concrete) {
            gc::tracer(
                self.context,
                self.module,
                self.program,
                shape.concrete,
                self.tracers,
            )?
            .as_global_value()
            .as_pointer_value()
        } else {
            pointer.const_null()
        };
        let size = self.builder.build_int_cast(
            native.size_of().ok_or("unsized dyn payload")?,
            self.size_type,
            "dyn.size",
        )?;
        let data = self
            .runtime_call(
                "box_new",
                Some(pointer.into()),
                &[size.into(), trace.into()],
            )?
            .ok_or("missing dyn allocation")?
            .into_pointer_value();
        self.restore_locals()?;
        let value = self.reload(source, value)?;
        // No allocation occurs between obtaining the fresh zeroed box and
        // storing its snapshot. The enclosing DynBox expression roots it next.
        self.builder.build_store(data, value)?;
        let ty =
            native_type(self.context, self.program, Type::Dyn(shape.interface))?.into_struct_type();
        let result = self
            .builder
            .build_insert_value(ty.const_zero(), data, 0, "dyn.data")?
            .into_struct_value();
        let result = self.builder.build_insert_value(
            result,
            self.witnesses[witness].ok_or("missing witness table")?,
            1,
            "dyn.witness",
        )?;
        Ok(Some(result.into_struct_value().into()))
    }

    pub(super) fn dyn_call(
        &mut self,
        receiver: &checked::Expr,
        slot: usize,
        arguments: &[checked::Expr],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let Type::Dyn(interface) = receiver.ty else {
            return Err("invalid dyn receiver type".into());
        };
        let Some(value) = self.expr(receiver)? else {
            return Ok(None);
        };
        let Some(arguments) = self.operands(arguments)? else {
            return Ok(None);
        };
        let receiver = self.reload(receiver, value)?.into_struct_value();
        let data = self.builder.build_extract_value(receiver, 0, "dyn.data")?;
        let witness = self
            .builder
            .build_extract_value(receiver, 1, "dyn.witness")?
            .into_pointer_value();
        let mut values = vec![data.into()];
        for argument in arguments {
            values.push(argument.into());
        }
        let interface = &self.program.interfaces[interface];
        let address = self.builder.build_struct_gep(
            table_type(self.context, interface.methods.len()),
            witness,
            u32::try_from(slot)?,
            "dyn.slot",
        )?;
        let method = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                address,
                "dyn.method",
            )?
            .into_pointer_value();
        let signature = &interface.methods[slot];
        let value = self
            .builder
            .build_indirect_call(
                method_type(self.context, self.program, signature)?,
                method,
                &values,
                if signature.result == Type::Unit {
                    ""
                } else {
                    "dyn.call"
                },
            )?
            .try_as_basic_value()
            .basic();
        self.restore_locals()?;
        Ok(value)
    }
}

#[cfg(test)]
#[path = "llvm_dyn_tests.rs"]
mod tests;
