//! Direct, checked access to the private Text and mutable Buffer layouts.

use super::*;

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    pub(super) fn memory_len(&self, header: PointerValue<'ctx>) -> NativeResult<IntValue<'ctx>> {
        // Text, Bytes and List begin with a target-sized length. Do not mark
        // this load invariant: Bytes/List aliases may mutate the same header.
        let length = self
            .builder
            .build_load(self.size_type, header, "memory.length")?
            .into_int_value();
        Ok(self.builder.build_int_cast_sign_flag(
            length,
            self.context.i64_type(),
            false,
            "memory.length.int",
        )?)
    }

    fn memory_index(
        &self,
        header: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        message: &str,
    ) -> NativeResult<IntValue<'ctx>> {
        let nonnegative = self.builder.build_int_compare(
            IntPredicate::SGE,
            index,
            self.context.i64_type().const_zero(),
            "index.nonnegative",
        )?;
        let in_range = self.builder.build_int_compare(
            IntPredicate::ULT,
            index,
            self.memory_len(header)?,
            "index.in.range",
        )?;
        self.guard(
            self.builder
                .build_and(nonnegative, in_range, "index.valid")?,
            message,
        )?;
        Ok(self
            .builder
            .build_int_cast(index, self.size_type, "memory.index")?)
    }

    pub(super) fn text_byte(
        &self,
        text: PointerValue<'ctx>,
        index: IntValue<'ctx>,
    ) -> NativeResult<IntValue<'ctx>> {
        let index = self.memory_index(text, index, "text byte index out of bounds")?;
        let layout = self.context.struct_type(
            &[
                self.size_type.into(),
                self.context.i8_type().array_type(0).into(),
            ],
            false,
        );
        let data = self
            .builder
            .build_struct_gep(layout, text, 1, "text.data")?;
        // SAFETY: Text stores length bytes immediately after its header, and
        // the guard established that index denotes one initialized byte.
        #[allow(unsafe_code)]
        let address = unsafe {
            self.builder.build_in_bounds_gep(
                self.context.i8_type(),
                data,
                &[index],
                "text.byte.address",
            )?
        };
        let byte = self
            .builder
            .build_load(self.context.i8_type(), address, "text.byte")?
            .into_int_value();
        Ok(self
            .builder
            .build_int_z_extend(byte, self.context.i64_type(), "text.byte.int")?)
    }

    pub(super) fn list_slot(
        &self,
        list: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        element: BasicTypeEnum<'ctx>,
    ) -> NativeResult<PointerValue<'ctx>> {
        let index = self.memory_index(list, index, "list index out of bounds")?;
        self.buffer_slot(list, index, element)
    }

    fn buffer_field(
        &self,
        buffer: PointerValue<'ctx>,
        index: u32,
    ) -> NativeResult<PointerValue<'ctx>> {
        let pointer = self.context.ptr_type(AddressSpace::default());
        let layout = self.context.struct_type(
            &[self.size_type.into(), self.size_type.into(), pointer.into()],
            false,
        );
        Ok(self
            .builder
            .build_struct_gep(layout, buffer, index, "buffer.field")?)
    }

    fn buffer_slot(
        &self,
        buffer: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        element: BasicTypeEnum<'ctx>,
    ) -> NativeResult<PointerValue<'ctx>> {
        let pointer = self.context.ptr_type(AddressSpace::default());
        let field = self.buffer_field(buffer, 2)?;
        let data = self
            .builder
            .build_load(pointer, field, "buffer.data")?
            .into_pointer_value();
        // SAFETY: The allocation uses this native element layout. Callers
        // either check index < len or ensure spare capacity for index == len.
        // All argument evaluation and any allocating reserve call precedes
        // this load; no user call/GC occurs before the slot is consumed.
        #[allow(unsafe_code)]
        Ok(unsafe {
            self.builder
                .build_in_bounds_gep(element, data, &[index], "buffer.slot")?
        })
    }

    pub(super) fn buffer_push(
        &self,
        buffer: PointerValue<'ctx>,
        mut item: BasicValueEnum<'ctx>,
        bytes: bool,
    ) -> NativeResult<()> {
        if bytes {
            let byte = item.into_int_value();
            let valid = self.builder.build_int_compare(
                IntPredicate::ULE,
                byte,
                self.context.i64_type().const_int(255, false),
                "byte.valid",
            )?;
            self.guard(valid, "byte value out of range")?;
            item = self
                .builder
                .build_int_truncate(byte, self.context.i8_type(), "byte")?
                .into();
        }
        let length = self
            .builder
            .build_load(self.size_type, buffer, "buffer.length")?
            .into_int_value();
        let capacity = self
            .builder
            .build_load(
                self.size_type,
                self.buffer_field(buffer, 1)?,
                "buffer.capacity",
            )?
            .into_int_value();
        let full =
            self.builder
                .build_int_compare(IntPredicate::EQ, length, capacity, "buffer.full")?;
        let grow = self
            .context
            .append_basic_block(self.function, "buffer.grow");
        let ready = self
            .context
            .append_basic_block(self.function, "buffer.ready");
        self.builder.build_conditional_branch(full, grow, ready)?;
        self.builder.position_at_end(grow);
        // Only capacity growth crosses the runtime/GC boundary. The header
        // and any managed fields in item are protected by expression roots.
        self.runtime_call(
            if bytes {
                "bytes_reserve_one"
            } else {
                "list_reserve_one"
            },
            None,
            &[buffer.into()],
        )?;
        self.builder.build_unconditional_branch(ready)?;
        self.builder.position_at_end(ready);
        let slot = self.buffer_slot(buffer, length, item.get_type())?;
        self.builder.build_store(slot, item)?;
        // reserve preserves length and checks len + 1; on the fast path
        // len < cap proves the same. Publish only after initializing the slot.
        let next = self.builder.build_int_nuw_add(
            length,
            self.size_type.const_int(1, false),
            "buffer.next.length",
        )?;
        self.builder.build_store(buffer, next)?;
        Ok(())
    }
}
