use std::fmt::{self, Write};

use loom_mir::{FunctionId, Type, WitnessId};

use crate::InstanceId;
use crate::ids::ProgramBrand;

/// Maximum number of type and witness nodes in one callable-instance key.
///
/// This is a compiler structural limit, not a source-language genericity
/// promise. Keeping one public budget gives builders, validators, identity
/// encoding, and future instance producers the same non-recursive bound.
pub const INSTANCE_KEY_STRUCTURE_BUDGET: usize = 256;

/// One proof argument captured by a callable instance.
///
/// The tree owns all of its children. It deliberately does not borrow checked
/// MIR: an instance plan remains self-contained after source lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceWitnessArgument {
    Concrete(WitnessId),
    Parameter(u32),
    Apply {
        witness: WitnessId,
        arguments: Box<[InstanceWitnessArgument]>,
    },
}

impl InstanceWitnessArgument {
    #[must_use]
    pub fn apply(witness: WitnessId, arguments: impl Into<Box<[InstanceWitnessArgument]>>) -> Self {
        Self::Apply {
            witness,
            arguments: arguments.into(),
        }
    }
}

/// Deterministic semantic identity of one lowered callable body.
///
/// Argument order is source-significant. No ordering or hashing contract is
/// required from [`Type`]; planning compares bounded owned keys directly and
/// uses their canonical iterative encoding for temporary compiler indexes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceKey {
    source: FunctionId,
    type_arguments: Box<[Type]>,
    witness_arguments: Box<[InstanceWitnessArgument]>,
}

impl InstanceKey {
    #[must_use]
    pub fn new(
        source: FunctionId,
        type_arguments: impl Into<Box<[Type]>>,
        witness_arguments: impl Into<Box<[InstanceWitnessArgument]>>,
    ) -> Self {
        Self {
            source,
            type_arguments: type_arguments.into(),
            witness_arguments: witness_arguments.into(),
        }
    }

    #[must_use]
    pub fn monomorphic(source: FunctionId) -> Self {
        Self {
            source,
            type_arguments: Box::default(),
            witness_arguments: Box::default(),
        }
    }

    #[must_use]
    pub const fn source(&self) -> FunctionId {
        self.source
    }

    #[must_use]
    pub const fn type_arguments(&self) -> &[Type] {
        &self.type_arguments
    }

    #[must_use]
    pub const fn witness_arguments(&self) -> &[InstanceWitnessArgument] {
        &self.witness_arguments
    }

    #[must_use]
    pub fn is_monomorphic(&self) -> bool {
        self.type_arguments.is_empty() && self.witness_arguments.is_empty()
    }

    pub(crate) fn validate_structure(&self) -> Result<(), InstanceKeyStructureError> {
        let root_count = self
            .type_arguments
            .len()
            .checked_add(self.witness_arguments.len())
            .ok_or(InstanceKeyStructureError::BudgetExceeded)?;
        if root_count > INSTANCE_KEY_STRUCTURE_BUDGET {
            return Err(InstanceKeyStructureError::BudgetExceeded);
        }
        let mut scheduled = root_count;
        let mut work = Vec::with_capacity(root_count);
        work.extend(
            self.type_arguments
                .iter()
                .rev()
                .map(InstanceStructureNode::Type),
        );
        work.extend(
            self.witness_arguments
                .iter()
                .rev()
                .map(InstanceStructureNode::Witness),
        );

        while let Some(node) = work.pop() {
            match node {
                InstanceStructureNode::Type(ty) => match ty {
                    Type::Tuple(elements) | Type::Nominal(_, elements) => {
                        schedule_types(elements, &mut scheduled, &mut work)?;
                    }
                    Type::List(element) | Type::Task(element) | Type::TaskOutcome(element) => {
                        schedule_types(
                            std::slice::from_ref(element.as_ref()),
                            &mut scheduled,
                            &mut work,
                        )?;
                    }
                    Type::View { bindings, .. } => {
                        let child_count = bindings.len();
                        require_scheduled_budget(&mut scheduled, child_count)?;
                        work.extend(bindings.values().rev().map(InstanceStructureNode::Type));
                    }
                    Type::Never
                    | Type::Unit
                    | Type::Bool
                    | Type::Int
                    | Type::Float
                    | Type::Text
                    | Type::Error => {}
                    Type::Parameter(_) | Type::AssociatedProjection { .. } => {
                        return Err(InstanceKeyStructureError::OpenArgument);
                    }
                },
                InstanceStructureNode::Witness(InstanceWitnessArgument::Apply {
                    arguments,
                    ..
                }) => {
                    require_scheduled_budget(&mut scheduled, arguments.len())?;
                    work.extend(arguments.iter().rev().map(InstanceStructureNode::Witness));
                }
                InstanceStructureNode::Witness(InstanceWitnessArgument::Concrete(_)) => {}
                InstanceStructureNode::Witness(InstanceWitnessArgument::Parameter(_)) => {
                    return Err(InstanceKeyStructureError::OpenArgument);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn canonical_identity(&self) -> String {
        let mut output = String::new();
        write!(&mut output, "{self}").expect("writing an instance key to a String cannot fail");
        output
    }
}

enum InstanceStructureNode<'a> {
    Type(&'a Type),
    Witness(&'a InstanceWitnessArgument),
}

fn schedule_types<'a>(
    types: &'a [Type],
    scheduled: &mut usize,
    work: &mut Vec<InstanceStructureNode<'a>>,
) -> Result<(), InstanceKeyStructureError> {
    require_scheduled_budget(scheduled, types.len())?;
    work.extend(types.iter().rev().map(InstanceStructureNode::Type));
    Ok(())
}

fn require_scheduled_budget(
    scheduled: &mut usize,
    additional: usize,
) -> Result<(), InstanceKeyStructureError> {
    *scheduled = scheduled
        .checked_add(additional)
        .ok_or(InstanceKeyStructureError::BudgetExceeded)?;
    if *scheduled > INSTANCE_KEY_STRUCTURE_BUDGET {
        return Err(InstanceKeyStructureError::BudgetExceeded);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstanceKeyStructureError {
    BudgetExceeded,
    OpenArgument,
}

impl fmt::Display for InstanceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "source=f{} types=[", self.source.0)?;
        for (index, ty) in self.type_arguments.iter().enumerate() {
            if index != 0 {
                formatter.write_char(',')?;
            }
            write_type_identity(formatter, ty)?;
        }
        formatter.write_str("] witnesses=[")?;
        for (index, witness) in self.witness_arguments.iter().enumerate() {
            if index != 0 {
                formatter.write_char(',')?;
            }
            write_witness(formatter, witness)?;
        }
        formatter.write_char(']')
    }
}

enum TypeWrite<'a> {
    Type(&'a Type),
    Text(&'static str),
    Name(&'a str),
}

/// Writes the complete, unambiguous identity of one MIR type.
///
/// Instance keys and the canonical LCIR dump share this encoder so adding a
/// type to the direct representation catalog cannot silently collapse two
/// artifact identities into an `<unsupported>` placeholder.
pub(crate) fn write_type_identity(output: &mut impl Write, root: &Type) -> fmt::Result {
    let mut work = vec![TypeWrite::Type(root)];
    while let Some(item) = work.pop() {
        match item {
            TypeWrite::Text(text) => output.write_str(text)?,
            TypeWrite::Name(name) => write!(output, "{}:{name}", name.len())?,
            TypeWrite::Type(ty) => match ty {
                Type::Never => output.write_str("Never")?,
                Type::Unit => output.write_str("Unit")?,
                Type::Bool => output.write_str("Bool")?,
                Type::Int => output.write_str("Int")?,
                Type::Float => output.write_str("Float")?,
                Type::Text => output.write_str("Text")?,
                Type::Tuple(elements) => {
                    output.write_str("Tuple[")?;
                    work.push(TypeWrite::Text("]"));
                    push_types(&mut work, elements);
                }
                Type::List(element) => {
                    output.write_str("List[")?;
                    work.push(TypeWrite::Text("]"));
                    work.push(TypeWrite::Type(element));
                }
                Type::Nominal(id, arguments) => {
                    write!(output, "Nominal#{}[", id.0)?;
                    work.push(TypeWrite::Text("]"));
                    push_types(&mut work, arguments);
                }
                Type::Parameter(index) => write!(output, "Parameter#{index}")?,
                Type::AssociatedProjection {
                    witness,
                    associated,
                } => {
                    write!(output, "Projection#{witness}(")?;
                    work.push(TypeWrite::Text(")"));
                    work.push(TypeWrite::Name(associated));
                }
                Type::Task(output_type) => {
                    output.write_str("Task[")?;
                    work.push(TypeWrite::Text("]"));
                    work.push(TypeWrite::Type(output_type));
                }
                Type::TaskOutcome(output_type) => {
                    output.write_str("TaskOutcome[")?;
                    work.push(TypeWrite::Text("]"));
                    work.push(TypeWrite::Type(output_type));
                }
                Type::View {
                    mutable,
                    concept,
                    bindings,
                } => {
                    write!(
                        output,
                        "View({}concept#{},{{",
                        if *mutable { "mut," } else { "" },
                        concept.0
                    )?;
                    work.push(TypeWrite::Text("})"));
                    for (index, (name, ty)) in bindings.iter().enumerate().rev() {
                        work.push(TypeWrite::Type(ty));
                        work.push(TypeWrite::Text("="));
                        work.push(TypeWrite::Name(name));
                        if index != 0 {
                            work.push(TypeWrite::Text(","));
                        }
                    }
                }
                Type::Error => output.write_str("Error")?,
            },
        }
    }
    Ok(())
}

fn push_types<'a>(work: &mut Vec<TypeWrite<'a>>, types: &'a [Type]) {
    for (index, ty) in types.iter().enumerate().rev() {
        work.push(TypeWrite::Type(ty));
        if index != 0 {
            work.push(TypeWrite::Text(","));
        }
    }
}

enum WitnessWrite<'a> {
    Witness(&'a InstanceWitnessArgument),
    Text(&'static str),
}

fn write_witness(output: &mut impl Write, root: &InstanceWitnessArgument) -> fmt::Result {
    let mut work = vec![WitnessWrite::Witness(root)];
    while let Some(item) = work.pop() {
        match item {
            WitnessWrite::Text(text) => output.write_str(text)?,
            WitnessWrite::Witness(InstanceWitnessArgument::Concrete(witness)) => {
                write!(output, "Concrete#{}", witness.0)?;
            }
            WitnessWrite::Witness(InstanceWitnessArgument::Parameter(index)) => {
                write!(output, "Parameter#{index}")?;
            }
            WitnessWrite::Witness(InstanceWitnessArgument::Apply { witness, arguments }) => {
                write!(output, "Apply#{}[", witness.0)?;
                work.push(WitnessWrite::Text("]"));
                for (index, argument) in arguments.iter().enumerate().rev() {
                    work.push(WitnessWrite::Witness(argument));
                    if index != 0 {
                        work.push(WitnessWrite::Text(","));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use loom_mir::{ConceptId, Type, TypeId};

    use super::write_type_identity;

    #[test]
    fn type_identity_encodes_every_mir_type_without_placeholders() {
        let bindings = BTreeMap::from([
            ("Item".to_owned(), Type::List(Box::new(Type::Text))),
            (
                "Output".to_owned(),
                Type::TaskOutcome(Box::new(Type::Error)),
            ),
        ]);
        let types = [
            Type::Never,
            Type::Unit,
            Type::Bool,
            Type::Int,
            Type::Float,
            Type::Text,
            Type::Tuple(vec![Type::Int, Type::Float]),
            Type::List(Box::new(Type::Bool)),
            Type::Nominal(TypeId(7), vec![Type::Parameter(2)]),
            Type::Parameter(3),
            Type::AssociatedProjection {
                witness: 4,
                associated: "Part".to_owned(),
            },
            Type::Task(Box::new(Type::Unit)),
            Type::TaskOutcome(Box::new(Type::Int)),
            Type::View {
                mutable: true,
                concept: ConceptId(5),
                bindings,
            },
            Type::Error,
        ];
        let mut output = String::new();
        for (index, ty) in types.iter().enumerate() {
            if index != 0 {
                output.push('|');
            }
            write_type_identity(&mut output, ty).expect("String formatting is infallible");
        }

        assert_eq!(
            output,
            "Never|Unit|Bool|Int|Float|Text|Tuple[Int,Float]|List[Bool]|Nominal#7[Parameter#2]|Parameter#3|Projection#4(4:Part)|Task[Unit]|TaskOutcome[Int]|View(mut,concept#5,{4:Item=List[Text],6:Output=TaskOutcome[Error]})|Error"
        );
        assert!(!output.contains("unsupported"));
    }
}

/// Dense entry in an [`InstancePlan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedInstance {
    pub(crate) id: InstanceId,
    pub(crate) key: InstanceKey,
}

impl PlannedInstance {
    #[must_use]
    pub const fn id(&self) -> InstanceId {
        self.id
    }

    #[must_use]
    pub const fn key(&self) -> &InstanceKey {
        &self.key
    }
}

/// Dense deterministic plan assigning semantic callable keys to LCIR ids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstancePlan {
    pub(crate) brand: ProgramBrand,
    pub(crate) entries: Vec<PlannedInstance>,
}

impl InstancePlan {
    pub(crate) const fn with_brand(brand: ProgramBrand) -> Self {
        Self {
            brand,
            entries: Vec::new(),
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[PlannedInstance] {
        &self.entries
    }

    #[must_use]
    pub fn key(&self, id: InstanceId) -> Option<&InstanceKey> {
        (id.brand() == self.brand)
            .then(|| self.entries.get(id.index()))
            .flatten()
            .filter(|entry| entry.id == id)
            .map(|entry| &entry.key)
    }

    #[must_use]
    pub fn find(&self, key: &InstanceKey) -> Option<InstanceId> {
        self.entries
            .iter()
            .find(|entry| entry.key == *key)
            .map(|entry| entry.id)
    }
}
