use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_PROGRAM_BRAND: AtomicUsize = AtomicUsize::new(1);

/// Process-local identity which prevents IDs from one LCIR program from being
/// interpreted in another. It is deliberately absent from textual output.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProgramBrand(usize);

impl ProgramBrand {
    pub(crate) fn fresh() -> Self {
        let raw = NEXT_PROGRAM_BRAND
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("LCIR program identity space is exhausted");
        Self(raw)
    }
}

impl fmt::Debug for ProgramBrand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProgramBrand(<private>)")
    }
}

macro_rules! define_global_id {
    ($($name:ident => $prefix:literal),+ $(,)?) => {
        $(
            #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
            pub struct $name {
                brand: ProgramBrand,
                raw: u32,
            }

            impl $name {
                #[must_use]
                pub(crate) fn from_index(brand: ProgramBrand, index: usize) -> Option<Self> {
                    u32::try_from(index).ok().map(|raw| Self { brand, raw })
                }

                #[must_use]
                pub(crate) const fn brand(self) -> ProgramBrand {
                    self.brand
                }

                #[must_use]
                pub const fn index(self) -> usize {
                    self.raw as usize
                }

                #[must_use]
                pub const fn raw(self) -> u32 {
                    self.raw
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(formatter, concat!($prefix, "{}"), self.raw)
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    fmt::Display::fmt(self, formatter)
                }
            }
        )+
    };
}

define_global_id!(
    InstanceId => "i",
    ValueTypeId => "t",
    ReprId => "r",
    ProductReprId => "p",
    SumReprId => "s",
);

macro_rules! define_local_id {
    ($($name:ident => $prefix:literal),+ $(,)?) => {
        $(
            /// Dense identity in one LCIR function instance.
            #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
            pub struct $name {
                owner: InstanceId,
                raw: u32,
            }

            impl $name {
                #[must_use]
                pub(crate) fn from_index(owner: InstanceId, index: usize) -> Option<Self> {
                    u32::try_from(index).ok().map(|raw| Self { owner, raw })
                }

                #[must_use]
                pub const fn owner(self) -> InstanceId {
                    self.owner
                }

                #[must_use]
                pub const fn index(self) -> usize {
                    self.raw as usize
                }

                #[must_use]
                pub const fn raw(self) -> u32 {
                    self.raw
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(formatter, concat!($prefix, "{}"), self.raw)
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    fmt::Display::fmt(self, formatter)
                }
            }
        )+
    };
}

define_local_id!(
    BlockId => "b",
    InstructionId => "n",
    ValueId => "v",
);
