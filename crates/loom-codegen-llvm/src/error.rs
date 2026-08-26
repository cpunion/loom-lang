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

impl From<loom_codegen_ir::GraphError> for CodegenError {
    fn from(error: loom_codegen_ir::GraphError) -> Self {
        Self::new(error.code().as_str(), error.message())
    }
}

#[cfg(test)]
mod tests {
    use loom_codegen_ir::{SourceRoots, analyze_source_reachability};
    use loom_mir::{FunctionId, Program};

    use super::CodegenError;

    #[test]
    fn source_graph_errors_cross_the_backend_boundary_losslessly() {
        let graph_error =
            analyze_source_reachability(&Program::default(), &SourceRoots::one(FunctionId(9)))
                .expect_err("missing source root must fail");

        let error = CodegenError::from(graph_error);

        assert_eq!(error.code(), "InvalidFunctionReference");
        assert_eq!(error.message(), "reachable function #9 does not exist");
    }
}
