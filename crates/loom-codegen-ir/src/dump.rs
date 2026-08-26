use std::fmt::{self, Write};

use loom_mir::Type;

use crate::{
    BlockTarget, BoolPredicate, CheckedIntBinaryOp, CheckedProgram, Constant, Effects,
    FloatBinaryOp, FloatPredicate, Instruction, InstructionKind, IntPredicate, Origin, Repr,
    ResultTarget, ScalarRepr, Terminator, TerminatorKind, UnwindTarget,
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
    writeln!(output, "lcir 0")?;
    writeln!(
        output,
        "target pointer_bits={}",
        representations.target().pointer_bits()
    )?;
    writeln!(output)?;
    for (index, repr) in representations.reprs().iter().enumerate() {
        writeln!(output, "repr r{index} = {}", repr_name(*repr))?;
    }
    writeln!(output)?;
    for (index, value_type) in representations.value_types().iter().enumerate() {
        writeln!(
            output,
            "type t{index} = {} => {}",
            type_name(value_type.semantic()),
            value_type.repr()
        )?;
    }

    for function in program.functions() {
        writeln!(output)?;
        write!(
            output,
            "fn {} mir=f{} {:?} (",
            function.id(),
            function.source().0,
            function.name()
        )?;
        for (index, parameter) in function.signature().params().iter().enumerate() {
            if index != 0 {
                write!(output, ", ")?;
            }
            write!(output, "{parameter}")?;
        }
        writeln!(
            output,
            ") -> {} effects={} {{",
            function.signature().result(),
            effects_name(function.effects())
        )?;
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
                write_instruction(output, instruction)?;
                if options.include_origins {
                    write_origin(output, instruction.origin(), " ; origin")?;
                } else {
                    writeln!(output)?;
                }
            }
            let terminator = block
                .terminator()
                .expect("checked LCIR block has a terminator");
            write_terminator(output, terminator)?;
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

fn write_instruction(output: &mut impl Write, instruction: &Instruction) -> fmt::Result {
    write!(output, "    {} ", instruction.id())?;
    if !instruction.results().is_empty() {
        for (index, result) in instruction.results().iter().enumerate() {
            if index != 0 {
                write!(output, ", ")?;
            }
            write!(output, "%{result}")?;
        }
        write!(output, " = ")?;
    }
    match instruction.kind() {
        InstructionKind::Constant(constant) => write_constant(output, *constant),
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

fn write_terminator(output: &mut impl Write, terminator: &Terminator) -> fmt::Result {
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
        TerminatorKind::Return(value) => write!(output, "return %{value}"),
        TerminatorKind::CheckedIntNegate {
            value,
            normal,
            fault,
        } => {
            write!(output, "checked_int.negate %{value}, normal ")?;
            write_result_target(output, normal)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault)
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
            write_result_target(output, normal)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault)
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
            write_result_target(output, normal)?;
            write!(output, ", unwind ")?;
            write_unwind_target(output, unwind)
        }
        TerminatorKind::Assert {
            condition,
            code,
            success,
            fault,
        } => {
            write!(output, "assert %{condition}, {code:?}, success ")?;
            write_target(output, success)?;
            write!(output, ", fault ")?;
            write_unwind_target(output, fault)
        }
        TerminatorKind::Fault { code } => write!(output, "fault {code:?}"),
        TerminatorKind::ResumeFault => write!(output, "resume_fault"),
    }
}

fn write_target(output: &mut impl Write, target: &BlockTarget) -> fmt::Result {
    write!(output, "{}(", target.block)?;
    write_arguments(output, &target.arguments)?;
    write!(output, ")")
}

fn write_result_target(output: &mut impl Write, target: &ResultTarget) -> fmt::Result {
    write!(output, "{}(result", target.block)?;
    if !target.arguments.is_empty() {
        write!(output, "; ")?;
        write_arguments(output, &target.arguments)?;
    }
    write!(output, ")")
}

fn write_unwind_target(output: &mut impl Write, target: &UnwindTarget) -> fmt::Result {
    write!(output, "{}(", target.block)?;
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

const fn repr_name(repr: Repr) -> &'static str {
    match repr {
        Repr::Uninhabited => "uninhabited",
        Repr::Zst => "zst",
        Repr::Scalar(ScalarRepr::I1) => "i1",
        Repr::Scalar(ScalarRepr::I64) => "i64",
        Repr::Scalar(ScalarRepr::F64) => "f64",
    }
}

fn type_name(ty: &Type) -> &'static str {
    match ty {
        Type::Never => "Never",
        Type::Unit => "Unit",
        Type::Bool => "Bool",
        Type::Int => "Int",
        Type::Float => "Float",
        Type::Text
        | Type::Tuple(_)
        | Type::List(_)
        | Type::Nominal(_, _)
        | Type::Parameter(_)
        | Type::AssociatedProjection { .. }
        | Type::Task(_)
        | Type::TaskOutcome(_)
        | Type::View { .. }
        | Type::Error => "<unsupported>",
    }
}

const fn effects_name(effects: Effects) -> &'static str {
    if effects.is_empty() {
        "none"
    } else if effects.contains(Effects::MAY_FAULT) {
        "may_fault"
    } else {
        "unknown"
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
