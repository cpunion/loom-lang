use std::error::Error;
use std::fmt;

/// A failure at the checked-MIR to native-artifact boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenError {
    code: &'static str,
    message: String,
}

impl CodegenError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CodegenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for CodegenError {}
