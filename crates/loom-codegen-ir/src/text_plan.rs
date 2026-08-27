/// Maximum UTF-8 byte length of one compiler-emitted immortal Text literal.
///
/// The limit is checked before LCIR clones source storage and is repeated by
/// independent validation before LLVM constructs a constant array.
pub const TEXT_LITERAL_MAX_BYTES: usize = 1024 * 1024;

/// Maximum UTF-8 bytes copied into all immortal Text literals in one LCIR
/// artifact. Generic instances are charged independently because each lowered
/// instruction is independently present in the artifact.
pub const TEXT_LITERAL_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct TextLiteralBudget {
    bytes: usize,
}

impl TextLiteralBudget {
    pub(crate) fn admit(&mut self, bytes: usize) -> bool {
        if bytes > TEXT_LITERAL_MAX_BYTES {
            return false;
        }
        let Some(total) = self.bytes.checked_add(bytes) else {
            return false;
        };
        if total > TEXT_LITERAL_MAX_TOTAL_BYTES {
            return false;
        }
        self.bytes = total;
        true
    }

    pub(crate) fn admit_all(&mut self, literals: impl IntoIterator<Item = usize>) -> bool {
        let mut added = 0_usize;
        for bytes in literals {
            if bytes > TEXT_LITERAL_MAX_BYTES {
                return false;
            }
            let Some(total) = added.checked_add(bytes) else {
                return false;
            };
            added = total;
        }
        let Some(total) = self.bytes.checked_add(added) else {
            return false;
        };
        if total > TEXT_LITERAL_MAX_TOTAL_BYTES {
            return false;
        }
        self.bytes = total;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_is_atomic_and_bounded_per_literal_and_artifact() {
        let mut budget = TextLiteralBudget::default();
        assert!(!budget.admit(TEXT_LITERAL_MAX_BYTES + 1));
        for _ in 0..(TEXT_LITERAL_MAX_TOTAL_BYTES / TEXT_LITERAL_MAX_BYTES) {
            assert!(budget.admit(TEXT_LITERAL_MAX_BYTES));
        }
        assert!(!budget.admit(1));
    }

    #[test]
    fn grouped_admission_is_atomic_and_keeps_individual_limits() {
        let mut budget = TextLiteralBudget::default();
        assert!(!budget.admit_all([1, TEXT_LITERAL_MAX_BYTES + 1]));
        assert!(budget.admit_all([TEXT_LITERAL_MAX_BYTES, TEXT_LITERAL_MAX_BYTES]));
        for _ in 2..(TEXT_LITERAL_MAX_TOTAL_BYTES / TEXT_LITERAL_MAX_BYTES) {
            assert!(budget.admit(TEXT_LITERAL_MAX_BYTES));
        }
        assert!(!budget.admit_all([1, 1]));
    }
}
