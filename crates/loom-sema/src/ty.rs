//! Interned semantic types and substitutions.

use std::collections::{BTreeMap, HashMap};

use loom_hir::{Arena, ArenaId, DefId, GenericParamId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TyId(u32);

impl TyId {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl ArenaId for TyId {
    fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    fn into_raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum BuiltinType {
    Bool,
    Int,
    Float,
    Text,
    Bytes,
    Path,
    Unit,
    ConstraintError,
    ContractFault,
    TaskFault,
    Duration,
    File,
    Socket,
    Json,
    JsonError,
    IoError,
    IoErrorKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Mutability {
    ReadOnly,
    Mutable,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ConceptInstance {
    pub concept: DefId,
    pub bindings: Vec<AssociatedTypeBinding>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct AssociatedTypeBinding {
    pub associated_type: DefId,
    pub ty: TyId,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum TyData {
    Error,
    Never,
    Builtin(BuiltinType),
    Tuple(Vec<TyId>),
    List(TyId),
    /// Immutable, canonically ordered Text-keyed map.
    TextMap(TyId),
    Option(TyId),
    Result {
        ok: TyId,
        error: TyId,
    },
    /// Compiler-known one-shot task handle. Source code may store and join it,
    /// but structured-consumption checks require it to be awaited, joined, or
    /// returned before its lexical scope exits.
    Task(TyId),
    TaskOutcome(TyId),
    Nominal {
        definition: DefId,
        arguments: Vec<TyId>,
    },
    Param(GenericParamId),
    /// `Self` while checking a concept requirement. Concrete impl/inherent
    /// methods substitute their resolved target type instead.
    SelfType(DefId),
    Projection {
        self_ty: TyId,
        concept: DefId,
        associated_type: DefId,
    },
    DynTarget(ConceptInstance),
    View {
        mutability: Mutability,
        target: TyId,
    },
}

/// A per-program canonical type store.
#[derive(Clone, Debug)]
pub struct TyInterner {
    values: Arena<TyId, TyData>,
    by_value: HashMap<TyData, TyId>,
    error: TyId,
    never: TyId,
}

#[derive(Deserialize, Serialize)]
struct TyInternerWire {
    values: Vec<TyData>,
    error: TyId,
    never: TyId,
}

impl Serialize for TyInterner {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        TyInternerWire {
            values: self.values.values().cloned().collect(),
            error: self.error,
            never: self.never,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TyInterner {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TyInternerWire::deserialize(deserializer)?;
        let mut values = Arena::default();
        let mut by_value = HashMap::new();
        for value in wire.values {
            let id = values.alloc(value.clone());
            if by_value.insert(value, id).is_some() {
                return Err(serde::de::Error::custom(
                    "typed cache contains a duplicate interned type",
                ));
            }
        }
        if values.get(wire.error) != Some(&TyData::Error)
            || values.get(wire.never) != Some(&TyData::Never)
        {
            return Err(serde::de::Error::custom(
                "typed cache has invalid error/never type identities",
            ));
        }
        Ok(Self {
            values,
            by_value,
            error: wire.error,
            never: wire.never,
        })
    }
}

impl Default for TyInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl TyInterner {
    #[must_use]
    pub fn new() -> Self {
        let mut values = Arena::default();
        let error = values.alloc(TyData::Error);
        let never = values.alloc(TyData::Never);
        let mut by_value = HashMap::new();
        by_value.insert(TyData::Error, error);
        by_value.insert(TyData::Never, never);
        Self {
            values,
            by_value,
            error,
            never,
        }
    }

    #[must_use]
    pub const fn error(&self) -> TyId {
        self.error
    }

    #[must_use]
    pub const fn never(&self) -> TyId {
        self.never
    }

    pub fn intern(&mut self, data: TyData) -> TyId {
        if let Some(existing) = self.by_value.get(&data) {
            return *existing;
        }
        let id = self.values.alloc(data.clone());
        self.by_value.insert(data, id);
        id
    }

    pub fn builtin(&mut self, builtin: BuiltinType) -> TyId {
        self.intern(TyData::Builtin(builtin))
    }

    #[must_use]
    pub fn data(&self, ty: TyId) -> &TyData {
        &self.values[ty]
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn substitute(&mut self, ty: TyId, substitution: &Substitution) -> TyId {
        match self.data(ty).clone() {
            TyData::Param(parameter) => substitution.get(parameter).unwrap_or(ty),
            TyData::Tuple(elements) => {
                let elements = elements
                    .into_iter()
                    .map(|element| self.substitute(element, substitution))
                    .collect();
                self.intern(TyData::Tuple(elements))
            }
            TyData::List(element) => {
                let element = self.substitute(element, substitution);
                self.intern(TyData::List(element))
            }
            TyData::TextMap(value) => {
                let value = self.substitute(value, substitution);
                self.intern(TyData::TextMap(value))
            }
            TyData::Nominal {
                definition,
                arguments,
            } => {
                let arguments = arguments
                    .into_iter()
                    .map(|argument| self.substitute(argument, substitution))
                    .collect();
                self.intern(TyData::Nominal {
                    definition,
                    arguments,
                })
            }
            TyData::Option(element) => {
                let element = self.substitute(element, substitution);
                self.intern(TyData::Option(element))
            }
            TyData::Result { ok, error } => {
                let ok = self.substitute(ok, substitution);
                let error = self.substitute(error, substitution);
                self.intern(TyData::Result { ok, error })
            }
            TyData::Task(output) => {
                let output = self.substitute(output, substitution);
                self.intern(TyData::Task(output))
            }
            TyData::TaskOutcome(output) => {
                let output = self.substitute(output, substitution);
                self.intern(TyData::TaskOutcome(output))
            }
            TyData::Projection {
                self_ty,
                concept,
                associated_type,
            } => {
                let self_ty = self.substitute(self_ty, substitution);
                self.intern(TyData::Projection {
                    self_ty,
                    concept,
                    associated_type,
                })
            }
            TyData::DynTarget(instance) => {
                let bindings = instance
                    .bindings
                    .into_iter()
                    .map(|binding| AssociatedTypeBinding {
                        associated_type: binding.associated_type,
                        ty: self.substitute(binding.ty, substitution),
                    })
                    .collect();
                self.intern(TyData::DynTarget(ConceptInstance {
                    concept: instance.concept,
                    bindings,
                }))
            }
            TyData::View { mutability, target } => {
                let target = self.substitute(target, substitution);
                self.intern(TyData::View { mutability, target })
            }
            TyData::Error | TyData::Never | TyData::Builtin(_) | TyData::SelfType(_) => ty,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Substitution {
    values: BTreeMap<GenericParamId, TyId>,
}

impl Substitution {
    pub fn insert(&mut self, parameter: GenericParamId, ty: TyId) -> Option<TyId> {
        self.values.insert(parameter, ty)
    }

    #[must_use]
    pub fn get(&self, parameter: GenericParamId) -> Option<TyId> {
        self.values.get(&parameter).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (GenericParamId, TyId)> + '_ {
        self.values.iter().map(|(parameter, ty)| (*parameter, *ty))
    }
}

#[cfg(test)]
mod tests {
    use loom_hir::{DefId, GenericParamId};

    use super::{Substitution, TyData, TyInterner};

    #[test]
    fn interning_and_recursive_substitution_are_canonical() {
        let mut types = TyInterner::new();
        let parameter = GenericParamId::from_raw(3);
        let parameter_ty = types.intern(TyData::Param(parameter));
        let text = types.intern(TyData::Nominal {
            definition: DefId::from_raw(10),
            arguments: Vec::new(),
        });
        let option_of_parameter = types.intern(TyData::Nominal {
            definition: DefId::from_raw(11),
            arguments: vec![parameter_ty],
        });
        let mut substitution = Substitution::default();
        substitution.insert(parameter, text);

        let instantiated = types.substitute(option_of_parameter, &substitution);
        let expected = types.intern(TyData::Nominal {
            definition: DefId::from_raw(11),
            arguments: vec![text],
        });
        assert_eq!(instantiated, expected);
    }
}
