//! Typed HIR identities.

use crate::ArenaId;
use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
            #[serde(transparent)]
            pub struct $name(u32);

            impl $name {
                #[must_use]
                pub const fn from_raw(raw: u32) -> Self {
                    Self(raw)
                }

                #[must_use]
                pub const fn raw(self) -> u32 {
                    self.0
                }
            }

            impl ArenaId for $name {
                fn from_raw(raw: u32) -> Self {
                    Self(raw)
                }

                fn into_raw(self) -> u32 {
                    self.0
                }
            }
        )+
    };
}

define_id!(
    ModuleId,
    DefId,
    GenericParamId,
    ParamId,
    TypeRefId,
    BodyId,
    LocalId,
    ExprId,
    PatternId,
);
