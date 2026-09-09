//! Callable values share {managed environment, entry}; direct calls keep their ABI.

use super::*;

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    pub(super) fn function_reference(
        &self,
        ty: Type,
        target: usize,
    ) -> NativeResult<BasicValueEnum<'ctx>> {
        let Type::Function(signature) = ty else {
            return Err("checked function reference needs a callable type".into());
        };
        let signature = &self.program.function_types[signature];
        let name = format!("loom.callable.{target}");
        let adapter = if let Some(adapter) = self.module.get_function(&name) {
            adapter
        } else {
            let adapter = self.module.add_function(
                &name,
                dynamic::method_type(self.context, self.program, signature)?,
                Some(Linkage::Internal),
            );
            let builder = self.context.create_builder();
            builder.position_at_end(self.context.append_basic_block(adapter, "entry"));
            let arguments = adapter
                .get_param_iter()
                .skip(1)
                .map(Into::into)
                .collect::<Vec<_>>();
            let result = builder.build_call(
                self.functions[target].ok_or("unresolved checked function reference")?,
                &arguments,
                if signature.result == Type::Unit {
                    ""
                } else {
                    "result"
                },
            )?;
            if let Some(result) = result.try_as_basic_value().basic() {
                builder.build_return(Some(&result))?;
            } else {
                builder.build_return(None)?;
            }
            adapter
        };
        Ok(self
            .context
            .const_struct(
                &[
                    self.context
                        .ptr_type(AddressSpace::default())
                        .const_null()
                        .into(),
                    adapter.as_global_value().as_pointer_value().into(),
                ],
                false,
            )
            .into())
    }
}
