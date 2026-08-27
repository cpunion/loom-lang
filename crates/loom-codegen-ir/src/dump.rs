use std::fmt::{self, Write};

use crate::instance::write_type_identity;
use crate::{
    BlockTarget, BoolPredicate, CheckedIntBinaryOp, CheckedProgram, Constant, FloatBinaryOp,
    FloatPredicate, Function, Instruction, InstructionKind, IntPredicate, Origin, Repr,
    ResultTarget, ScalarRepr, SumTagRepr, Terminator, TerminatorKind, UnwindTarget, ValueTypeKind,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DumpOptions {
    pub include_origins: bool,
}

/// Returns a deterministic textual form of validated LCIR.
///
/// # Panics
///
/// Panics only if Rust's infallible `String` formatting implementation reports
/// an error.
#[must_use]
pub fn dump_program(program: &CheckedProgram) -> String {
    let mut output = String::new();
    write_program(program, &mut output).expect("writing LCIR to a String cannot fail");
    output
}

/// Writes deterministic LCIR without source origins.
///
/// # Errors
///
/// Returns the destination writer's formatting error.
pub fn write_program(program: &CheckedProgram, output: &mut impl Write) -> fmt::Result {
    write_program_with_options(program, DumpOptions::default(), output)
}

/// Writes deterministic LCIR with explicit dump options.
///
/// # Errors
///
/// Returns the destination writer's formatting error.
///
/// # Panics
///
/// Panics only if the supplied [`CheckedProgram`] violates its private checked
/// boundary. Safe public APIs cannot construct such a value.
#[allow(clippy::too_many_lines)]
pub fn write_program_with_options(
    program: &CheckedProgram,
    options: DumpOptions,
    output: &mut impl Write,
) -> fmt::Result {
    let program = program.as_program();
    let representations = program.representations();
    writeln!(output, "lcir 26")?;
    writeln!(
        output,
        "target pointer_bits={}",
        representations.target().pointer_bits()
    )?;
    writeln!(output)?;
    for (index, repr) in representations.reprs().iter().enumerate() {
        write!(output, "repr r{index} = ")?;
        write_repr(output, representations, *repr)?;
        writeln!(output)?;
    }
    writeln!(output)?;
    for (index, value_type) in representations.value_types().iter().enumerate() {
        write!(output, "type t{index} = ")?;
        write_type_identity(output, value_type.semantic())?;
        write!(output, " => {}", value_type.repr())?;
        match value_type.kind() {
            ValueTypeKind::Direct => {}
            ValueTypeKind::ManagedTextMap => output.write_str(" managed_text_map")?,
            ValueTypeKind::Transparent { base } => write!(output, " transparent({base})")?,
            ValueTypeKind::InvariantProduct => output.write_str(" invariant_product")?,
        }
        writeln!(output)?;
    }
    writeln!(output)?;
    for (index, registration) in representations.registrations().iter().enumerate() {
        write!(output, "registration k{index} = ")?;
        write_type_identity(output, registration.semantic())?;
        writeln!(output, " => {}", registration.value_type())?;
    }
    for (index, dynamic) in representations.dynamics().iter().enumerate() {
        write!(output, "dynamic d{index} = {} candidates=[", dynamic.view())?;
        for (candidate_index, candidate) in dynamic.candidates().iter().enumerate() {
            if candidate_index != 0 {
                write!(output, ", ")?;
            }
            write!(output, "{candidate}")?;
        }
        writeln!(output, "]")?;
    }

    for instance in program.instances().entries() {
        writeln!(output)?;
        writeln!(output, "instance {} = {}", instance.id(), instance.key())?;
    }

    for function in program.functions() {
        writeln!(output)?;
        write!(output, "fn {} mir=f{} ", function.id(), function.source().0)?;
        write_quoted_string(output, function.name())?;
        write!(output, " (")?;
        for (index, parameter) in function.signature().params().iter().enumerate() {
            if index != 0 {
                write!(output, ", ")?;
            }
            write!(output, "{parameter}")?;
        }
        write!(output, ") -> {}", function.signature().result())?;
        if !function.signature().inout_params().is_empty() {
            write!(output, " inout=[")?;
            for (index, parameter) in function.signature().inout_params().iter().enumerate() {
                if index != 0 {
                    write!(output, ", ")?;
                }
                write!(output, "{parameter}")?;
            }
            write!(output, "]")?;
        }
        let entry = function
            .entry()
            .expect("checked LCIR function has an entry block");
        write!(output, " entry={entry} effects={}", function.effects())?;
        if let Some(coroutine) = function.coroutine() {
            write!(output, " coroutine output={} states=[", coroutine.output())?;
            for (index, suspension) in coroutine.suspensions().iter().enumerate() {
                if index != 0 {
                    write!(output, ", ")?;
                }
                write!(output, "{} awaited=(", suspension.state())?;
                for (awaited_index, ty) in suspension.awaited().iter().enumerate() {
                    if awaited_index != 0 {
                        write!(output, ", ")?;
                    }
                    write!(output, "{ty}")?;
                }
                write!(output, ") live=(")?;
                for (live_index, ty) in suspension.live().iter().enumerate() {
                    if live_index != 0 {
                        write!(output, ", ")?;
                    }
                    write!(output, "{ty}")?;
                }
                write!(output, ")")?;
            }
            write!(output, "]")?;
        }
        writeln!(output, " {{")?;
        if options.include_origins {
            write_origin(output, function.origin(), "  ; function-origin")?;
        }
        for (block_index, block) in function.blocks().iter().enumerate() {
            write!(output, "  {}", block.id())?;
            if !block.params().is_empty() {
                write!(output, "(")?;
                for (index, value) in block.params().iter().copied().enumerate() {
                    if index != 0 {
                        write!(output, ", ")?;
                    }
                    let ty = function
                        .value(value)
                        .expect("checked LCIR block parameter exists")
                        .ty();
                    write!(output, "%{value}: {ty}")?;
                }
                write!(output, ")")?;
            }
            writeln!(output, ":")?;
            for instruction_id in block.instructions() {
                let instruction = function
                    .instruction(*instruction_id)
                    .expect("checked LCIR instruction exists");
                write_instruction(output, function, instruction)?;
                if options.include_origins {
                    write_origin(output, instruction.origin(), " ; origin")?;
                } else {
                    writeln!(output)?;
                }
            }
            let terminator = block
                .terminator()
                .expect("checked LCIR block has a terminator");
            write_terminator(output, program, terminator)?;
            if options.include_origins {
                write_origin(output, terminator.origin(), " ; origin")?;
            } else {
                writeln!(output)?;
            }
            if block_index + 1 != function.blocks().len() {
                writeln!(output)?;
            }
        }
        writeln!(output, "}}")?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the canonical instruction encoder keeps every opcode spelling in one exhaustive match"
)]
fn write_instruction(
    output: &mut impl Write,
    function: &Function,
    instruction: &Instruction,
) -> fmt::Result {
    write!(output, "    {} ", instruction.id())?;
    if !instruction.results().is_empty() {
        for (index, result) in instruction.results().iter().enumerate() {
            if index != 0 {
                write!(output, ", ")?;
            }
            let ty = function
                .value(*result)
                .expect("checked LCIR instruction result exists")
                .ty();
            write!(output, "%{result}: {ty}")?;
        }
        write!(output, " = ")?;
    }
    match instruction.kind() {
        InstructionKind::Constant(constant) => write_constant(output, *constant),
        InstructionKind::TextLiteral { utf8 } => {
            output.write_str("text.literal ")?;
            write_quoted_string(output, utf8)
        }
        InstructionKind::TextConcat { left, right } => {
            write!(output, "text.concat %{left}, %{right}")
        }
        InstructionKind::TextGet {
            text,
            index,
            missing_variant,
            found_variant,
        } => write!(
            output,
            "text.get %{text}, %{index}, missing {missing_variant}, found {found_variant}"
        ),
        InstructionKind::TextLength { text } => write!(output, "text.length %{text}"),
        InstructionKind::TextContains { text, needle } => {
            write!(output, "text.contains %{text}, %{needle}")
        }
        InstructionKind::TextCompare {
            predicate,
            left,
            right,
        } => write!(
            output,
            "text.compare.{} %{left}, %{right}",
            bool_predicate_name(*predicate)
        ),
        InstructionKind::ParseInt {
            text,
            ok_variant,
            error_variant,
            invalid_syntax_variant,
            out_of_range_variant,
        } => write!(
            output,
            "parse.int %{text}, ok {ok_variant}, error {error_variant}, invalid_syntax {invalid_syntax_variant}, out_of_range {out_of_range_variant}"
        ),
        InstructionKind::ParseFloat {
            text,
            ok_variant,
            error_variant,
            invalid_syntax_variant,
            out_of_range_variant,
        } => write!(
            output,
            "parse.float %{text}, ok {ok_variant}, error {error_variant}, invalid_syntax {invalid_syntax_variant}, out_of_range {out_of_range_variant}"
        ),
        InstructionKind::FormatFloat { value } => write!(output, "format.float %{value}"),
        InstructionKind::ProductConstruct { fields } => {
            write!(output, "product.construct (")?;
            write_arguments(output, fields)?;
            write!(output, ")")
        }
        InstructionKind::InvariantRecordProven { fields } => {
            write!(output, "invariant_record.proven (")?;
            write_arguments(output, fields)?;
            write!(output, ")")
        }
        InstructionKind::ProductExtract { aggregate, field } => {
            write!(output, "product.extract %{aggregate}, field {field}")
        }
        InstructionKind::ProductInsert {
            aggregate,
            field,
            value,
        } => write!(
            output,
            "product.insert %{aggregate}, field {field}, %{value}"
        ),
        InstructionKind::InvariantReceiverInsert {
            aggregate,
            field,
            value,
        } => write!(
            output,
            "invariant_receiver.insert %{aggregate}, field {field}, %{value}"
        ),
        InstructionKind::RefineProven { value } => write!(output, "refine.proven %{value}"),
        InstructionKind::Unrefine { value } => write!(output, "unrefine %{value}"),
        InstructionKind::SumConstruct { variant, payload } => {
            write!(output, "sum.construct variant {variant} (")?;
            write_arguments(output, payload)?;
            write!(output, ")")
        }
        InstructionKind::DynConstruct { variant, value } => {
            write!(output, "dyn.construct variant {variant} %{value}")
        }
        InstructionKind::ListConstruct { elements } => {
            write!(output, "list.construct (")?;
            write_arguments(output, elements)?;
            write!(output, ")")
        }
        InstructionKind::ListAppend { list, value } => {
            write!(output, "list.append %{list}, %{value}")
        }
        InstructionKind::ListAppendUnique { list, value } => {
            write!(output, "list.append.unique %{list}, %{value}")
        }
        InstructionKind::ListLength { list } => write!(output, "list.length %{list}"),
        InstructionKind::ListGet { list, index } => {
            write!(output, "list.get %{list}, %{index}")
        }
        InstructionKind::TextMapConstruct => output.write_str("text_map.construct"),
        InstructionKind::TextMapInsert { map, key, value } => {
            write!(output, "text_map.insert %{map}, %{key}, %{value}")
        }
        InstructionKind::TextMapLength { map } => write!(output, "text_map.length %{map}"),
        InstructionKind::TextMapContains { map, key } => {
            write!(output, "text_map.contains %{map}, %{key}")
        }
        InstructionKind::TextMapGet { map, key } => {
            write!(output, "text_map.get %{map}, %{key}")
        }
        InstructionKind::TextMapRemove { map, key } => {
            write!(output, "text_map.remove %{map}, %{key}")
        }
        InstructionKind::TextMapEntryGet { map, index } => {
            write!(output, "text_map.entry_get %{map}, %{index}")
        }
        InstructionKind::BoolNot { value } => write!(output, "bool.not %{value}"),
        InstructionKind::BoolCompare {
            predicate,
            left,
            right,
        } => write!(
            output,
            "bool.compare.{} %{left}, %{right}",
            bool_predicate_name(*predicate)
        ),
        InstructionKind::FloatNegate { value } => write!(output, "float.negate %{value}"),
        InstructionKind::FloatBinary {
            op, left, right, ..
        } => write!(output, "float.{} %{left}, %{right}", float_binary_name(*op)),
        InstructionKind::IntCompare {
            predicate,
            left,
            right,
        } => write!(
            output,
            "int.compare.{} %{left}, %{right}",
            int_predicate_name(*predicate)
        ),
        InstructionKind::IntSuccessorBelow {
            value,
            upper_bound,
            proof,
        } => write!(
            output,
            "int.successor_below %{value}, upper %{upper_bound}, proof %{proof}"
        ),
        InstructionKind::FloatCompare {
            predicate,
            left,
            right,
        } => write!(
            output,
            "float.compare.{} %{left}, %{right}",
            float_predicate_name(*predicate)
        ),
        InstructionKind::DirectCall { callee, arguments } => {
            write!(output, "call {callee}(")?;
            for (index, argument) in arguments.iter().enumerate() {
                if index != 0 {
                    write!(output, ", ")?;
                }
                write!(output, "%{argument}")?;
            }
            write!(output, ")")
        }
        InstructionKind::TaskCreate {
            coroutine,
            arguments,
        } => {
            write!(output, "task.create {coroutine}(")?;
            write_arguments(output, arguments)?;
            write!(output, ")")
        }
        InstructionKind::TaskJoinAll { tasks } => {
            write!(output, "task.join_all(")?;
            write_arguments(output, tasks)?;
            write!(output, ")")
        }
    }
}

fn write_constant(output: &mut impl Write, constant: Constant) -> fmt::Result {
    match constant {
        Constant::Unit => write!(output, "const unit"),
        Constant::Bool(value) => write!(output, "const bool {value}"),
        Constant::Int(value) => write!(output, "const int {value}"),
        Constant::FloatBits(bits) => write!(output, "const float 0x{bits:016x}"),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the canonical textual schema keeps every terminator spelling in one exhaustive match"
)]
fn write_terminator(
    output: &mut impl Write,
    program: &crate::Program,
    terminator: &Terminator,
) -> fmt::Result {
    write!(output, "    ")?;
    match terminator.kind() {
        TerminatorKind::Jump(target) => {
            write!(output, "jump ")?;
            write_target(output, target)
        }
        TerminatorKind::Branch {
            condition,
            then_target,
            else_target,
        } => {
            write!(output, "branch %{condition}, ")?;
            write_target(output, then_target)?;
            write!(output, ", ")?;
            write_target(output, else_target)
        }
        TerminatorKind::SumSwitch { scrutinee, cases } => {
            write!(output, "sum.switch %{scrutinee}")?;
            let payloads = program
                .function(scrutinee.owner())
                .and_then(|function| function.value(*scrutinee))
                .and_then(|value| program.representations().value_type(value.ty()))
                .and_then(|value_type| program.representations().repr(value_type.repr()))
                .and_then(|repr| match repr {
                    Repr::Sum(sum) => program.representations().sum(*sum),
                    Repr::Uninhabited
                    | Repr::Zst
                    | Repr::Scalar(_)
                    | Repr::ImmortalText
                    | Repr::ManagedPointer
                    | Repr::TaskHandle
                    | Repr::Product(_) => None,
                })
                .map(crate::SumRepr::variants);
            for (index, case) in cases.iter().enumerate() {
                write!(output, ", case {} => {}(", case.variant, case.block)?;
                let payload_count = payloads
                    .and_then(|variants| variants.get(index))
                    .map(|variant| variant.fields().len())
                    .unwrap_or_default();
                for payload in 0..payload_count {
                    if payload != 0 {
                        write!(output, ", ")?;
                    }
                    write!(output, "payload{payload}")?;
                }
                if payload_count != 0 && !case.arguments.is_empty() {
                    write!(output, "; ")?;
                }
                write_arguments(output, &case.arguments)?;
                write!(output, ")")?;
            }
            Ok(())
        }
        TerminatorKind::DynSwitch { scrutinee, cases } => {
            write!(output, "dyn.switch %{scrutinee}")?;
            let candidates = program
                .function(scrutinee.owner())
                .and_then(|function| function.value(*scrutinee))
                .and_then(|value| program.representations().dynamic(value.ty()))
                .map(crate::DynamicRepr::candidates);
            for (index, case) in cases.iter().enumerate() {
                write!(output, ", case {} => {}(", case.variant, case.block)?;
                if candidates.and_then(|values| values.get(index)).is_some() {
                    write!(output, "<payload>")?;
                    if !case.arguments.is_empty() {
                        write!(output, ", ")?;
                    }
                }
                write_arguments(output, &case.arguments)?;
                write!(output, ")")?;
            }
            Ok(())
        }
        TerminatorKind::Return(value) => write!(output, "return %{value}"),
        TerminatorKind::TaskSleep {
            milliseconds,
            normal,
            fault,
        } => {
            write!(output, "task.sleep %{milliseconds}, normal ")?;
            write_result_target(output, normal, 0)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault, 0)
        }
        TerminatorKind::AwaitTasks {
            state,
            tasks,
            normal,
        } => {
            write!(output, "await_tasks state {state}, (")?;
            write_arguments(output, tasks)?;
            write!(output, "), normal ")?;
            write_await_result_target(output, normal, tasks.len())
        }
        TerminatorKind::CheckedIntNegate {
            value,
            normal,
            fault,
        } => {
            write!(output, "checked_int.negate %{value}, normal ")?;
            write_result_target(output, normal, 0)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault, 0)
        }
        TerminatorKind::CheckedIntBinary {
            op,
            left,
            right,
            normal,
            fault,
        } => {
            write!(
                output,
                "checked_int.{} %{left}, %{right}, normal ",
                checked_int_binary_name(*op)
            )?;
            write_result_target(output, normal, 0)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault, 0)
        }
        TerminatorKind::Invoke {
            callee,
            arguments,
            normal,
            unwind,
        } => {
            write!(output, "invoke {callee}(")?;
            write_arguments(output, arguments)?;
            write!(output, "), normal ")?;
            let writebacks = program
                .function(*callee)
                .map(|callee| callee.signature().inout_params().len())
                .unwrap_or_default();
            write_result_target(output, normal, writebacks)?;
            write!(output, ", unwind ")?;
            write_unwind_target(output, unwind, writebacks)
        }
        TerminatorKind::ResourceClose {
            kind,
            resource,
            normal,
            fault,
        } => {
            let kind = match kind {
                crate::ResourceKind::File => "file",
                crate::ResourceKind::Socket => "socket",
            };
            write!(output, "resource.close.{kind} %{resource}, normal ")?;
            write_result_target(output, normal, 1)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault, 1)
        }
        TerminatorKind::Assert {
            condition,
            metadata,
            success,
            fault,
        } => {
            write!(output, "assert %{condition}, ")?;
            write_fault_metadata(output, metadata)?;
            write!(output, ", success ")?;
            write_target(output, success)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault, 0)
        }
        TerminatorKind::Fault { metadata } => {
            write!(output, "fault ")?;
            write_fault_metadata(output, metadata)
        }
        TerminatorKind::ResumeFault => write!(output, "resume_fault"),
    }?;
    if !terminator.writebacks().is_empty() {
        write!(output, " writebacks(")?;
        write_arguments(output, terminator.writebacks())?;
        write!(output, ")")?;
    }
    Ok(())
}

fn write_target(output: &mut impl Write, target: &BlockTarget) -> fmt::Result {
    write!(output, "{}(", target.block)?;
    write_arguments(output, &target.arguments)?;
    write!(output, ")")
}

fn write_result_target(
    output: &mut impl Write,
    target: &ResultTarget,
    writebacks: usize,
) -> fmt::Result {
    write!(output, "{}(result", target.block)?;
    for index in 0..writebacks {
        write!(output, ", writeback{index}")?;
    }
    if !target.arguments.is_empty() {
        write!(output, "; ")?;
        write_arguments(output, &target.arguments)?;
    }
    write!(output, ")")
}

fn write_await_result_target(
    output: &mut impl Write,
    target: &ResultTarget,
    results: usize,
) -> fmt::Result {
    write!(output, "{}(", target.block)?;
    for index in 0..results {
        if index != 0 {
            write!(output, ", ")?;
        }
        write!(output, "result{index}")?;
    }
    if !target.arguments.is_empty() {
        if results != 0 {
            write!(output, "; ")?;
        }
        write_arguments(output, &target.arguments)?;
    }
    write!(output, ")")
}

fn write_unwind_target(
    output: &mut impl Write,
    target: &UnwindTarget,
    writebacks: usize,
) -> fmt::Result {
    write!(output, "{}(", target.block)?;
    for index in 0..writebacks {
        if index != 0 {
            write!(output, ", ")?;
        }
        write!(output, "writeback{index}")?;
    }
    if writebacks != 0 && !target.arguments.is_empty() {
        write!(output, "; ")?;
    }
    write_arguments(output, &target.arguments)?;
    write!(output, ")")
}

fn write_arguments(output: &mut impl Write, arguments: &[crate::ValueId]) -> fmt::Result {
    for (index, argument) in arguments.iter().enumerate() {
        if index != 0 {
            write!(output, ", ")?;
        }
        write!(output, "%{argument}")?;
    }
    Ok(())
}

fn write_quoted_string(output: &mut impl Write, value: &str) -> fmt::Result {
    output.write_char('"')?;
    for character in value.chars() {
        match character {
            '"' => output.write_str("\\\"")?,
            '\\' => output.write_str("\\\\")?,
            '\n' => output.write_str("\\n")?,
            '\r' => output.write_str("\\r")?,
            '\t' => output.write_str("\\t")?,
            character if character.is_control() => {
                write!(output, "\\u{{{:x}}}", u32::from(character))?;
            }
            character => output.write_char(character)?,
        }
    }
    output.write_char('"')
}

fn write_origin(output: &mut impl Write, origin: Origin, prefix: &str) -> fmt::Result {
    write!(output, "{prefix} f{}", origin.source_function.0)?;
    if let Some(expression) = origin.expression {
        write!(output, "/e{}", expression.0)?;
    }
    writeln!(
        output,
        " file{}:{}..{}",
        origin.span.file.0, origin.span.range.start, origin.span.range.end
    )
}

fn write_repr(
    output: &mut impl Write,
    representations: &crate::RepresentationPlan,
    repr: Repr,
) -> fmt::Result {
    match repr {
        Repr::Uninhabited => output.write_str("uninhabited"),
        Repr::Zst => output.write_str("zst"),
        Repr::Scalar(ScalarRepr::I1) => output.write_str("i1"),
        Repr::Scalar(ScalarRepr::I64) => output.write_str("i64"),
        Repr::Scalar(ScalarRepr::F64) => output.write_str("f64"),
        Repr::ImmortalText => output.write_str("immortal_text_ptr"),
        Repr::ManagedPointer => output.write_str("managed_ptr"),
        Repr::TaskHandle => output.write_str("task_handle"),
        Repr::Product(product) => {
            write!(output, "product {product}(")?;
            let fields = representations
                .product(product)
                .expect("checked LCIR product representation exists")
                .fields();
            for (index, field) in fields.iter().enumerate() {
                if index != 0 {
                    output.write_str(", ")?;
                }
                write!(output, "{field}")?;
            }
            output.write_char(')')
        }
        Repr::Sum(sum) => {
            let sum_repr = representations
                .sum(sum)
                .expect("checked LCIR sum representation exists");
            write!(output, "sum {sum} tag={} [", sum_tag_name(sum_repr.tag()))?;
            for (variant_index, variant) in sum_repr.variants().iter().enumerate() {
                if variant_index != 0 {
                    output.write_str(", ")?;
                }
                write!(output, "{variant_index}(")?;
                for (field_index, field) in variant.fields().iter().enumerate() {
                    if field_index != 0 {
                        output.write_str(", ")?;
                    }
                    write!(output, "{field}")?;
                }
                output.write_char(')')?;
            }
            output.write_char(']')
        }
    }
}

const fn sum_tag_name(tag: SumTagRepr) -> &'static str {
    match tag {
        SumTagRepr::Tagless => "tagless",
        SumTagRepr::I8 => "i8",
        SumTagRepr::I16 => "i16",
        SumTagRepr::I32 => "i32",
    }
}

const fn float_binary_name(op: FloatBinaryOp) -> &'static str {
    match op {
        FloatBinaryOp::Add => "add",
        FloatBinaryOp::Subtract => "subtract",
        FloatBinaryOp::Multiply => "multiply",
        FloatBinaryOp::Divide => "divide",
    }
}

const fn bool_predicate_name(predicate: BoolPredicate) -> &'static str {
    match predicate {
        BoolPredicate::Equal => "equal",
        BoolPredicate::NotEqual => "not_equal",
    }
}

const fn checked_int_binary_name(op: CheckedIntBinaryOp) -> &'static str {
    match op {
        CheckedIntBinaryOp::Add => "add",
        CheckedIntBinaryOp::Subtract => "subtract",
        CheckedIntBinaryOp::Multiply => "multiply",
        CheckedIntBinaryOp::Divide => "divide",
    }
}

const fn int_predicate_name(predicate: IntPredicate) -> &'static str {
    match predicate {
        IntPredicate::Equal => "equal",
        IntPredicate::NotEqual => "not_equal",
        IntPredicate::Less => "less",
        IntPredicate::LessEqual => "less_equal",
        IntPredicate::Greater => "greater",
        IntPredicate::GreaterEqual => "greater_equal",
    }
}

const fn float_predicate_name(predicate: FloatPredicate) -> &'static str {
    match predicate {
        FloatPredicate::OrderedEqual => "ordered_equal",
        FloatPredicate::UnorderedNotEqual => "unordered_not_equal",
        FloatPredicate::OrderedLess => "ordered_less",
        FloatPredicate::OrderedLessEqual => "ordered_less_equal",
        FloatPredicate::OrderedGreater => "ordered_greater",
        FloatPredicate::OrderedGreaterEqual => "ordered_greater_equal",
    }
}

const fn fault_code_name(code: crate::FaultCode) -> &'static str {
    match code {
        crate::FaultCode::ArtifactProofRejected => "ArtifactProofRejected",
        crate::FaultCode::IntegerOverflow => "IntegerOverflow",
        crate::FaultCode::IntegerDivisionByZero => "IntegerDivisionByZero",
        crate::FaultCode::IntegerDivisionOverflow => "IntegerDivisionOverflow",
        crate::FaultCode::InvalidDuration => "InvalidDuration",
        crate::FaultCode::InvalidSleepDuration => "InvalidSleepDuration",
        crate::FaultCode::SleepDurationOverflow => "SleepDurationOverflow",
        crate::FaultCode::ResourceClose => "ResourceCloseFault",
    }
}

fn write_fault_metadata(output: &mut impl Write, metadata: &crate::FaultMetadata) -> fmt::Result {
    match metadata {
        crate::FaultMetadata::Runtime(code) => {
            write!(output, "runtime {}", fault_code_name(*code))
        }
        crate::FaultMetadata::Contract(metadata) => write_contract_fault(output, metadata),
    }
}

fn write_contract_fault(
    output: &mut impl Write,
    metadata: &crate::ContractFaultMetadata,
) -> fmt::Result {
    write!(
        output,
        "contract {} category={} user_code=",
        metadata.kind().fault_code(),
        metadata.kind().category()
    )?;
    if let Some(user_code) = metadata.user_code() {
        write_quoted_string(output, user_code)?;
    } else {
        output.write_str("none")?;
    }
    output.write_str(" message=")?;
    write_quoted_string(output, metadata.message())?;
    output.write_str(" contract_span=")?;
    write_span(output, metadata.contract_span())?;
    output.write_str(" blame_span=")?;
    write_span(output, metadata.blame_span())
}

fn write_span(output: &mut impl Write, span: loom_core::Span) -> fmt::Result {
    write!(
        output,
        "file{}:{}..{}",
        span.file.0, span.range.start, span.range.end
    )
}
