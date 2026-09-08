//! Emission of already-lowered typed frames and irreducible task operations.
//! State splitting and spill selection happen in Loom, not in this backend.

use super::*;

pub(super) fn present(program: &checked::Program, reachable: &BTreeSet<usize>) -> bool {
    reachable.iter().any(|id| {
        let mut values = Vec::new();
        gc::block_expressions(&program.functions[*id].body, &mut values);
        values.iter().any(|value| {
            matches!(
                value.kind,
                checked::ExprKind::Primitive(Primitive::TaskCreate | Primitive::TaskRun, _)
            )
        })
    })
}

pub(super) fn frame_layout<'ctx>(
    context: &'ctx Context,
    program: &checked::Program,
    ty: Type,
) -> NativeResult<inkwell::types::StructType<'ctx>> {
    let Type::Data(id) = ty else {
        return Err("invalid frame type".into());
    };
    let checked::DataKind::Frame(fields) = &program.types[id].kind else {
        return Err("expected a generated frame layout".into());
    };
    let fields = fields
        .iter()
        .map(|(_, field)| native_type(context, program, *field))
        .collect::<NativeResult<Vec<_>>>()?;
    Ok(context.struct_type(&fields, false))
}

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    pub(super) fn frame_new(
        &mut self,
        ty: Type,
        fields: &[(usize, checked::Expr)],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let Some(values) = self.operands(fields.iter().map(|(_, value)| value))? else {
            return Ok(None);
        };
        let layout = frame_layout(self.context, self.program, ty)?;
        let trace = gc::frame_tracer(self.context, self.module, self.program, ty)?;
        let size = self.builder.build_int_cast(
            layout.size_of().ok_or("unsized frame")?,
            self.size_type,
            "frame.size",
        )?;
        let pointer = self.context.ptr_type(AddressSpace::default());
        let frame = self
            .runtime_call(
                "box_new",
                Some(pointer.into()),
                &[
                    size.into(),
                    trace.as_global_value().as_pointer_value().into(),
                ],
            )?
            .ok_or("missing frame allocation")?
            .into_pointer_value();
        self.restore_locals()?;
        // Zeroed fields are traceable before first resume; copy initializer
        // snapshots only after the allocation has relocated existing values.
        for ((index, source), value) in fields.iter().zip(values) {
            let value = self.reload(source, value)?;
            let address =
                self.builder
                    .build_struct_gep(layout, frame, *index as u32, "frame.initializer")?;
            self.builder.build_store(address, value)?;
        }
        Ok(Some(frame.into()))
    }

    pub(super) fn frame_store(
        &mut self,
        frame: &checked::Expr,
        field: usize,
        value: &checked::Expr,
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        // The frame base, never an interior address, survives value evaluation.
        let Some(values) = self.operands([frame, value])? else {
            return Ok(None);
        };
        let address = self.builder.build_struct_gep(
            frame_layout(self.context, self.program, frame.ty)?,
            values[0].into_pointer_value(),
            field as u32,
            "frame.store.field",
        )?;
        self.builder.build_store(address, values[1])?;
        Ok(None)
    }

    pub(super) fn task_primitive(
        &mut self,
        result: Type,
        operation: Primitive,
        values: &[BasicValueEnum<'ctx>],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let pointer = self.context.ptr_type(AddressSpace::default());
        match operation {
            Primitive::TaskCreate => {
                // Lowering supplies a static creation-site Text. The runtime
                // borrows its compiler-owned bytes, never a managed interior.
                let label = values[2].into_pointer_value();
                let layout = self.context.struct_type(
                    &[
                        self.size_type.into(),
                        self.context.i8_type().array_type(0).into(),
                    ],
                    false,
                );
                let length = self
                    .builder
                    .build_load(self.size_type, label, "task.site.length")?;
                let bytes = self
                    .builder
                    .build_struct_gep(layout, label, 1, "task.site.bytes")?;
                self.runtime_call(
                    "task_create",
                    Some(self.context.i64_type().into()),
                    &[values[0], values[1], bytes.into(), length],
                )
            }
            Primitive::TaskAwait | Primitive::TaskWaitTimer => {
                let operation = if operation == Primitive::TaskAwait {
                    "task_await"
                } else {
                    "task_wait_timer"
                };
                let ready = self
                    .runtime_call(operation, Some(self.context.i32_type().into()), values)?
                    .ok_or("missing task readiness")?
                    .into_int_value();
                Ok(Some(
                    self.builder
                        .build_int_compare(
                            IntPredicate::NE,
                            ready,
                            self.context.i32_type().const_zero(),
                            "task.ready",
                        )?
                        .into(),
                ))
            }
            Primitive::TaskResult => {
                let frame = self
                    .runtime_call("task_result", Some(pointer.into()), values)?
                    .ok_or("missing completed frame")?
                    .into_pointer_value();
                if result == Type::Unit {
                    return Ok(None);
                }
                // Every frame for Task[T] begins with precisely T. No GC or
                // child-root removal occurs before the receiver snapshots it.
                Ok(Some(self.builder.build_load(
                    native_type(self.context, self.program, result)?,
                    frame,
                    "task.result",
                )?))
            }
            Primitive::TaskRelease => self.runtime_call("task_release", None, values),
            Primitive::TaskRun => {
                let output = self.runtime_call("task_run", None, values)?;
                self.restore_locals()?;
                Ok(output)
            }
            _ => Err("expected a lowered task primitive".into()),
        }
    }
}
