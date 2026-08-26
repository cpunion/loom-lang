use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    Block, CallArgument, CheckedProgram, Constant, Contract, ContractExpr, ContractExprKind, Expr,
    ExprKind, MirValidationErrors, Pattern, Program, StatementKind, TypeDefKind, check_program,
};

pub const INTERPRETED_ARTIFACT_FORMAT: &str = "loom.interpreted-mir";
pub const INTERPRETED_ARTIFACT_VERSION: u32 = 17;
pub const LOOM_LANGUAGE_VERSION: &str = loom_core::LOOM_LANGUAGE_VERSION;
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const MAX_ARTIFACT_JSON_NESTING: usize = 512;

#[derive(Debug)]
pub enum ArtifactError {
    Encode(String),
    Malformed(String),
    FormatMismatch {
        expected: &'static str,
        found: String,
    },
    VersionMismatch {
        expected: u32,
        found: u64,
    },
    LanguageVersionMismatch {
        expected: &'static str,
        found: String,
    },
    MissingEntry,
    UnknownEntry {
        entry: String,
    },
    FloatTableMismatch {
        slots: usize,
        entries: usize,
    },
    NonCanonicalNaN {
        index: usize,
        bits: u64,
    },
    InvalidProgram(MirValidationErrors),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(message) => write!(formatter, "could not encode MIR artifact: {message}"),
            Self::Malformed(message) => write!(formatter, "malformed MIR artifact: {message}"),
            Self::FormatMismatch { expected, found } => {
                write!(formatter, "artifact format `{found}` is not `{expected}`")
            }
            Self::VersionMismatch { expected, found } => write!(
                formatter,
                "artifact version {found} is incompatible with supported version {expected}"
            ),
            Self::LanguageVersionMismatch { expected, found } => write!(
                formatter,
                "artifact language version `{found}` is incompatible with supported version `{expected}`"
            ),
            Self::MissingEntry => write!(formatter, "executable artifact has no fixed entry"),
            Self::UnknownEntry { entry } => {
                write!(
                    formatter,
                    "artifact entry `{entry}` is not exported by the program"
                )
            }
            Self::FloatTableMismatch { slots, entries } => write!(
                formatter,
                "artifact contains {slots} Float slot(s) but {entries} bit-table entry/entries"
            ),
            Self::NonCanonicalNaN { index, bits } => write!(
                formatter,
                "Float table entry {index} contains non-canonical NaN bits 0x{bits:016x}"
            ),
            Self::InvalidProgram(errors) => errors.fmt(formatter),
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidProgram(errors) => Some(errors),
            _ => None,
        }
    }
}

impl From<MirValidationErrors> for ArtifactError {
    fn from(errors: MirValidationErrors) -> Self {
        Self::InvalidProgram(errors)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Envelope {
    format: String,
    version: u32,
    language_version: String,
    entry: Option<String>,
    program: Program,
    float_bits: Vec<u64>,
}

/// Encodes a checked, interpreted MIR artifact entirely in memory.
///
/// The byte representation is deterministic: structs have fixed field order,
/// maps are `BTreeMap`s, and every Float is represented in a traversal-ordered
/// `u64` side table. All NaN spellings collapse to one quiet-NaN bit pattern.
///
/// # Errors
///
/// Returns [`ArtifactError::Encode`] on serialization failure. Unchecked MIR
/// cannot enter this public encoding boundary.
pub fn encode_interpreted_artifact(program: &CheckedProgram) -> Result<Vec<u8>, ArtifactError> {
    encode_interpreted_artifact_envelope(program, None)
}

/// Encodes an interpreted executable whose entry is fixed at build time.
///
/// # Errors
///
/// Returns [`ArtifactError::UnknownEntry`] when `entry` is not exported, in
/// addition to serialization failures. Unchecked MIR cannot enter this public
/// encoding boundary.
pub fn encode_interpreted_executable_artifact(
    program: &CheckedProgram,
    entry: &str,
) -> Result<Vec<u8>, ArtifactError> {
    encode_interpreted_artifact_envelope(program, Some(entry))
}

fn encode_interpreted_artifact_envelope(
    checked: &CheckedProgram,
    entry: Option<&str>,
) -> Result<Vec<u8>, ArtifactError> {
    let program = checked.as_program();
    validate_entry(program, entry)?;
    let mut normalized = program.clone();
    let mut float_bits = Vec::new();
    visit_program_constants(&mut normalized, &mut |constant| {
        if let Constant::Float(value) = constant {
            float_bits.push(canonical_float_bits(*value));
            *value = 0.0;
        }
    });
    let bytes = serde_json::to_vec(&Envelope {
        format: INTERPRETED_ARTIFACT_FORMAT.to_owned(),
        version: INTERPRETED_ARTIFACT_VERSION,
        language_version: LOOM_LANGUAGE_VERSION.to_owned(),
        entry: entry.map(str::to_owned),
        program: normalized,
        float_bits,
    })
    .map_err(|error| ArtifactError::Encode(error.to_string()))?;
    validate_json_nesting(&bytes)
        .map_err(|message| ArtifactError::Encode(format!("wire nesting: {message}")))?;
    Ok(bytes)
}

/// Decodes, version-checks, restores Float bits, and validates an interpreted
/// MIR artifact. No filesystem state participates in decoding.
///
/// # Errors
///
/// Rejects malformed envelopes, format/version mismatches, non-canonical Float
/// tables, and any decoded program which fails MIR validation.
pub fn decode_interpreted_artifact(bytes: &[u8]) -> Result<CheckedProgram, ArtifactError> {
    decode_interpreted_artifact_envelope(bytes).map(|(program, _entry)| program)
}

/// Decodes an interpreted executable and returns its build-time entry.
///
/// # Errors
///
/// Returns [`ArtifactError::MissingEntry`] for a validated MIR/cache artifact
/// which was not built as an executable, or any ordinary artifact error.
pub fn decode_interpreted_executable_artifact(
    bytes: &[u8],
) -> Result<(CheckedProgram, String), ArtifactError> {
    let (program, entry) = decode_interpreted_artifact_envelope(bytes)?;
    let entry = entry.ok_or(ArtifactError::MissingEntry)?;
    Ok((program, entry))
}

fn decode_interpreted_artifact_envelope(
    bytes: &[u8],
) -> Result<(CheckedProgram, Option<String>), ArtifactError> {
    validate_json_nesting(bytes).map_err(ArtifactError::Malformed)?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    deserializer.disable_recursion_limit();
    let value = serde_json::Value::deserialize(&mut deserializer)
        .map_err(|error| ArtifactError::Malformed(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ArtifactError::Malformed(error.to_string()))?;
    validate_header(&value)?;
    let mut envelope: Envelope = serde_json::from_value(value)
        .map_err(|error| ArtifactError::Malformed(error.to_string()))?;

    let slots = count_program_floats(&envelope.program);
    if slots != envelope.float_bits.len() {
        return Err(ArtifactError::FloatTableMismatch {
            slots,
            entries: envelope.float_bits.len(),
        });
    }
    for (index, bits) in envelope.float_bits.iter().copied().enumerate() {
        if f64::from_bits(bits).is_nan() && bits != CANONICAL_NAN_BITS {
            return Err(ArtifactError::NonCanonicalNaN { index, bits });
        }
    }
    let mut bits = envelope.float_bits.into_iter();
    visit_program_constants(&mut envelope.program, &mut |constant| {
        if let Constant::Float(value) = constant
            && let Some(next) = bits.next()
        {
            *value = f64::from_bits(next);
        }
    });
    let program = check_program(envelope.program).map_err(ArtifactError::InvalidProgram)?;
    validate_entry(program.as_program(), envelope.entry.as_deref())?;
    Ok((program, envelope.entry))
}

fn validate_entry(program: &Program, entry: Option<&str>) -> Result<(), ArtifactError> {
    if let Some(entry) = entry
        && !program.exports.contains_key(entry)
    {
        return Err(ArtifactError::UnknownEntry {
            entry: entry.to_owned(),
        });
    }
    Ok(())
}

fn validate_json_nesting(bytes: &[u8]) -> Result<(), String> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > MAX_ARTIFACT_JSON_NESTING {
                    return Err(format!("JSON nesting exceeds {MAX_ARTIFACT_JSON_NESTING}"));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn validate_header(value: &serde_json::Value) -> Result<(), ArtifactError> {
    let object = value
        .as_object()
        .ok_or_else(|| ArtifactError::Malformed("top level must be an object".to_owned()))?;
    let format = object
        .get("format")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ArtifactError::Malformed("missing string field `format`".to_owned()))?;
    if format != INTERPRETED_ARTIFACT_FORMAT {
        return Err(ArtifactError::FormatMismatch {
            expected: INTERPRETED_ARTIFACT_FORMAT,
            found: format.to_owned(),
        });
    }
    let version = object
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ArtifactError::Malformed("missing integer field `version`".to_owned()))?;
    if version != u64::from(INTERPRETED_ARTIFACT_VERSION) {
        return Err(ArtifactError::VersionMismatch {
            expected: INTERPRETED_ARTIFACT_VERSION,
            found: version,
        });
    }
    let language_version = object
        .get("languageVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ArtifactError::Malformed("missing string field `languageVersion`".to_owned())
        })?;
    if language_version != LOOM_LANGUAGE_VERSION {
        return Err(ArtifactError::LanguageVersionMismatch {
            expected: LOOM_LANGUAGE_VERSION,
            found: language_version.to_owned(),
        });
    }
    Ok(())
}

fn canonical_float_bits(value: f64) -> u64 {
    if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        value.to_bits()
    }
}

fn count_program_floats(program: &Program) -> usize {
    let mut copy = program.clone();
    let mut count = 0;
    visit_program_constants(&mut copy, &mut |constant| {
        if matches!(constant, Constant::Float(_)) {
            count += 1;
        }
    });
    count
}

fn visit_program_constants(program: &mut Program, visitor: &mut impl FnMut(&mut Constant)) {
    for definition in &mut program.types {
        match &mut definition.kind {
            TypeDefKind::Record { invariant, .. } => {
                if let Some(invariant) = invariant {
                    visit_contract(invariant, visitor);
                }
            }
            TypeDefKind::Refined { predicate, .. } => visit_contract(predicate, visitor),
            TypeDefKind::Enum { .. } => {}
        }
    }
    for function in &mut program.functions {
        if let Some(invariant) = &mut function.call_plan.receiver_invariant {
            visit_contract(invariant, visitor);
        }
        for contract in &mut function.call_plan.requires {
            visit_contract(contract, visitor);
        }
        for contract in &mut function.call_plan.ensures {
            visit_contract(contract, visitor);
        }
        visit_block(&mut function.body, visitor);
    }
}

fn visit_contract(contract: &mut Contract, visitor: &mut impl FnMut(&mut Constant)) {
    visit_contract_expr(&mut contract.expression, visitor);
}

fn visit_contract_expr(expression: &mut ContractExpr, visitor: &mut impl FnMut(&mut Constant)) {
    match &mut expression.kind {
        ContractExprKind::Constant(constant) => visitor(constant),
        ContractExprKind::Field(value, _) | ContractExprKind::Unary(_, value) => {
            visit_contract_expr(value, visitor);
        }
        ContractExprKind::Binary(_, left, right) => {
            visit_contract_expr(left, visitor);
            visit_contract_expr(right, visitor);
        }
        ContractExprKind::IsFinite(value) => visit_contract_expr(value, visitor),
        ContractExprKind::Match { scrutinee, arms } => {
            visit_contract_expr(scrutinee, visitor);
            for arm in arms {
                visit_pattern(&mut arm.pattern, visitor);
                visit_contract_expr(&mut arm.value, visitor);
            }
        }
        ContractExprKind::Value(_) | ContractExprKind::Binding(_) => {}
    }
}

fn visit_block(block: &mut Block, visitor: &mut impl FnMut(&mut Constant)) {
    for statement in &mut block.statements {
        match &mut statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Assert { condition: value }
            | StatementKind::Evaluate(value) => visit_expr(value, visitor),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                visit_expr(start, visitor);
                visit_expr(end, visitor);
                visit_block(body, visitor);
            }
            StatementKind::Defer(block) => visit_block(block, visitor),
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    visit_expr(value, visitor);
                }
            }
        }
    }
    if let Some(tail) = &mut block.tail {
        visit_expr(tail, visitor);
    }
}

fn visit_expr(expression: &mut Expr, visitor: &mut impl FnMut(&mut Constant)) {
    match &mut expression.kind {
        ExprKind::Constant(constant) => visitor(constant),
        ExprKind::Unary(_, value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitIo { source: value, .. } => {
            visit_expr(value, visitor);
        }
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                visit_expr(element, visitor);
            }
        }
        ExprKind::Binary(_, left, right) => {
            visit_expr(left, visitor);
            visit_expr(right, visitor);
        }
        ExprKind::Block(block) => visit_block(block, visitor),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expr(condition, visitor);
            visit_block(then_branch, visitor);
            visit_block(else_branch, visitor);
        }
        ExprKind::Match { scrutinee, arms } => {
            visit_expr(scrutinee, visitor);
            for arm in arms {
                visit_pattern(&mut arm.pattern, visitor);
                visit_expr(&mut arm.value, visitor);
            }
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                visit_expr(field, visitor);
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                visit_expr(value, visitor);
            }
        }
        ExprKind::Call { arguments, .. } => {
            for argument in arguments {
                if let CallArgument::Value(value) = argument {
                    visit_expr(value, visitor);
                }
            }
        }
        ExprKind::TaskJoin { arguments, .. } => {
            for argument in arguments {
                visit_expr(argument, visitor);
            }
        }
        ExprKind::Copy(_) | ExprKind::Move(_) | ExprKind::ReborrowView { .. } => {}
        ExprKind::MakeView { value, .. } => visit_expr(value, visitor),
    }
}

fn visit_pattern(pattern: &mut Pattern, visitor: &mut impl FnMut(&mut Constant)) {
    match pattern {
        Pattern::Constant(constant) => visitor(constant),
        Pattern::Variant { payload, .. } => {
            for nested in payload {
                visit_pattern(nested, visitor);
            }
        }
        Pattern::Wildcard | Pattern::Binding => {}
    }
}
