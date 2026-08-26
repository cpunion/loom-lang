//! Native LLVM target-machine policy shared by emission and cache identity.

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::module::{FlagBehavior, Module};
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use serde::Serialize;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

use crate::CodegenError;

// An implicit host target is tuned for the current machine. Supplying any target triple is the
// explicit opt-in to a portable object, including when that triple happens to name the host.
const PORTABLE_CPU: &str = "generic";
const PORTABLE_CPU_FEATURES: &str = "";
pub const DEVELOPMENT_OPTIMIZATION_PIPELINE: &str = "default<O0>,globaldce";
pub const RELEASE_OPTIMIZATION_PIPELINE: &str = "default<O2>,globaldce";
pub const RELOCATION_MODE: &str = "pic";
pub const NATIVE_RUNTIME_ABI: &str = loom_runtime_abi::NATIVE_RUNTIME_ABI_IDENTITY;

/// Adds the target object format's debug metadata module flags.
///
/// LLVM's common `DI*` metadata is encoded as `CodeView` for an MSVC target only
/// when the `CodeView` module flag is present. Other supported native targets
/// use DWARF metadata.
pub(crate) fn configure_debug_module_flags<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    target_triple: &str,
) {
    module.add_basic_value_flag(
        "Debug Info Version",
        FlagBehavior::Warning,
        context.i32_type().const_int(3, false),
    );
    let (format, version) = if crate::target_uses_msvc_artifacts(Some(target_triple)) {
        ("CodeView", 1)
    } else {
        ("Dwarf Version", 4)
    };
    module.add_basic_value_flag(
        format,
        FlagBehavior::Warning,
        context.i32_type().const_int(version, false),
    );
}

/// User-selected LLVM optimization policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationProfile {
    #[default]
    Development,
    Release,
}

impl OptimizationProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Release => "release",
        }
    }

    #[must_use]
    pub const fn pipeline(self) -> &'static str {
        match self {
            Self::Development => DEVELOPMENT_OPTIMIZATION_PIPELINE,
            Self::Release => RELEASE_OPTIMIZATION_PIPELINE,
        }
    }

    const fn llvm_level(self) -> OptimizationLevel {
        match self {
            Self::Development => OptimizationLevel::None,
            Self::Release => OptimizationLevel::Default,
        }
    }
}

/// LLVM facts which affect native object and executable compatibility.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTargetIdentity {
    pub triple: String,
    pub data_layout: String,
    pub cpu_policy: String,
    pub cpu_features: String,
    pub optimization: String,
    pub relocation: String,
}

/// The exact target-machine inputs selected for one object emission.
pub(crate) struct NativeTargetMachine {
    pub(crate) triple: TargetTriple,
    pub(crate) machine: TargetMachine,
    cpu: String,
    features: String,
    optimization: OptimizationProfile,
    explicit: bool,
}

impl NativeTargetMachine {
    pub(crate) fn identity(&self) -> NativeTargetIdentity {
        let data_layout = self.machine.get_target_data().get_data_layout();
        NativeTargetIdentity {
            triple: self.triple.as_str().to_string_lossy().into_owned(),
            data_layout: data_layout.as_str().to_string_lossy().into_owned(),
            cpu_policy: self.cpu.clone(),
            cpu_features: self.features.clone(),
            optimization: self.optimization.pipeline().to_owned(),
            relocation: RELOCATION_MODE.to_owned(),
        }
    }

    pub(crate) const fn optimization(&self) -> OptimizationProfile {
        self.optimization
    }

    pub(crate) const fn target_selection(&self) -> &'static str {
        if self.explicit {
            "explicit"
        } else {
            "implicit-host"
        }
    }

    pub(crate) fn pointer_bits(&self) -> Result<u32, CodegenError> {
        self.machine
            .get_target_data()
            .get_pointer_byte_size(None)
            .checked_mul(8)
            .ok_or_else(|| {
                CodegenError::new(
                    "NativePointerWidthOverflow",
                    format!("target {} pointer width does not fit u32 bits", self.triple),
                )
            })
    }

    pub(crate) fn validate_legacy_value_abi(&self) -> Result<(), CodegenError> {
        let pointer_bits = self.pointer_bits()?;
        if pointer_bits == 64 {
            return Ok(());
        }
        Err(CodegenError::new(
            "UnsupportedNativePointerWidth",
            format!(
                "target {} uses {pointer_bits}-bit pointers; the current native Value ABI requires 64-bit pointers",
                self.triple
            ),
        ))
    }
}

/// Returns the identity of the exact target-machine policy used for emission.
///
/// # Errors
///
/// Returns a stable backend error if native LLVM target initialization fails.
pub fn native_target_identity() -> Result<NativeTargetIdentity, CodegenError> {
    target_identity(None, OptimizationProfile::Development)
}

/// Returns the exact target-machine identity for a host or explicit LLVM triple.
///
/// # Errors
///
/// Returns a stable backend error if LLVM does not provide the requested target.
pub fn target_identity(
    triple: Option<&str>,
    optimization: OptimizationProfile,
) -> Result<NativeTargetIdentity, CodegenError> {
    let target = create_target_machine(triple, optimization)?;
    Ok(target.identity())
}

/// Links Mach-O object debug sections into a standard dSYM bundle.
///
/// Linux ELF executables already carry their DWARF. An MSVC link writes the
/// object-level `CodeView` records into a sibling PDB, so this function validates
/// that companion instead of invoking a second post-link tool. The presence of
/// a PDB alone does not imply function-level source metadata.
///
/// # Errors
///
/// Returns a stable backend error when dSYM generation or atomic replacement
/// fails.
pub fn emit_native_debug_companion(executable: &Path) -> Result<(), CodegenError> {
    #[cfg(target_os = "macos")]
    {
        let parent = executable.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| {
            CodegenError::new(
                "DebugInfoWriteFailed",
                format!("{}: {error}", parent.display()),
            )
        })?;
        let temporary = tempfile::tempdir_in(parent)
            .map_err(|error| CodegenError::new("DebugInfoWriteFailed", error.to_string()))?;
        let generated = temporary.path().join("loom.dSYM");
        let tool = std::env::var_os("LOOM_DSYMUTIL").unwrap_or_else(|| "dsymutil".into());
        let result = std::process::Command::new(&tool)
            .arg("--verify")
            .arg("-o")
            .arg(&generated)
            .arg(executable)
            .output()
            .map_err(|error| {
                CodegenError::new(
                    "DebugInfoToolUnavailable",
                    format!("{}: {error}", Path::new(&tool).display()),
                )
            })?;
        if !result.status.success() {
            return Err(CodegenError::new(
                "DebugInfoWriteFailed",
                String::from_utf8_lossy(&result.stderr).trim().to_owned(),
            ));
        }
        let destination = native_debug_bundle_path(executable);
        if destination.exists() {
            std::fs::remove_dir_all(&destination).map_err(|error| {
                CodegenError::new(
                    "DebugInfoWriteFailed",
                    format!("{}: {error}", destination.display()),
                )
            })?;
        }
        std::fs::rename(&generated, &destination).map_err(|error| {
            CodegenError::new(
                "DebugInfoWriteFailed",
                format!("{}: {error}", destination.display()),
            )
        })?;
    }
    #[cfg(target_env = "msvc")]
    {
        let pdb =
            crate::native_artifact_path(executable, None, crate::NativeArtifactKind::DebugDatabase);
        if !pdb.is_file() {
            return Err(CodegenError::new(
                "DebugInfoWriteFailed",
                format!("linker did not produce {}", pdb.display()),
            ));
        }
    }
    #[cfg(not(any(target_os = "macos", target_env = "msvc")))]
    let _ = executable;
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_debug_bundle_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".dSYM");
    PathBuf::from(name)
}

pub(crate) fn create_target_machine(
    requested: Option<&str>,
    optimization: OptimizationProfile,
) -> Result<NativeTargetMachine, CodegenError> {
    let target = create_llvm_target_machine(requested, optimization)?;
    target.validate_legacy_value_abi()?;
    Ok(target)
}

/// Creates the representation-neutral LLVM target machine shared by emitters.
///
/// Callers which need a particular runtime or value ABI must enforce that
/// policy after comparing the target data against their checked IR layout.
pub(crate) fn create_llvm_target_machine(
    requested: Option<&str>,
    optimization: OptimizationProfile,
) -> Result<NativeTargetMachine, CodegenError> {
    Target::initialize_all(&InitializationConfig::default());
    let host_native = requested.is_none();
    let triple = requested.map_or_else(TargetMachine::get_default_triple, |triple| {
        TargetMachine::normalize_triple(&TargetTriple::create(triple))
    });
    let (cpu, features) = if host_native {
        (
            TargetMachine::get_host_cpu_name().to_string(),
            TargetMachine::get_host_cpu_features().to_string(),
        )
    } else {
        (PORTABLE_CPU.to_owned(), PORTABLE_CPU_FEATURES.to_owned())
    };
    let target = Target::from_triple(&triple)
        .map_err(|message| CodegenError::new("LlvmTargetUnavailable", message.to_string()))?;
    let machine = target
        .create_target_machine(
            &triple,
            &cpu,
            &features,
            optimization.llvm_level(),
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| {
            CodegenError::new(
                "LlvmTargetUnavailable",
                format!("LLVM could not create a target machine for {triple}"),
            )
        })?;
    Ok(NativeTargetMachine {
        triple,
        machine,
        cpu,
        features,
        optimization,
        explicit: requested.is_some(),
    })
}

/// Reports whether an explicit triple normalizes to the current host triple.
#[must_use]
pub fn is_native_target(requested: Option<&str>) -> bool {
    let Some(requested) = requested else {
        return true;
    };
    let requested = TargetMachine::normalize_triple(&TargetTriple::create(requested));
    let native = TargetMachine::normalize_triple(&TargetMachine::get_default_triple());
    requested == native
}

#[cfg(test)]
mod tests {
    use super::*;

    fn debug_flag_ir(target: &str) -> String {
        let context = Context::create();
        let module = context.create_module("debug.flags");
        configure_debug_module_flags(&context, &module, target);
        module.print_to_string().to_string()
    }

    #[test]
    fn msvc_debug_metadata_selects_codeview_without_dwarf() {
        let ir = debug_flag_ir("x86_64-pc-windows-msvc");
        assert!(ir.contains("CodeView"), "{ir}");
        assert!(!ir.contains("Dwarf Version"), "{ir}");
        assert!(ir.contains("Debug Info Version"), "{ir}");
    }

    #[test]
    fn non_msvc_debug_metadata_selects_dwarf_without_codeview() {
        let ir = debug_flag_ir("x86_64-unknown-linux-gnu");
        assert!(ir.contains("Dwarf Version"), "{ir}");
        assert!(!ir.contains("CodeView"), "{ir}");
        assert!(ir.contains("Debug Info Version"), "{ir}");
    }
}
