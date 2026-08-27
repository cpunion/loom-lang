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

use crate::{CodegenError, trace_llvm_stage};

// An implicit host target is tuned for the current machine. Supplying any target triple is the
// explicit opt-in to a portable object, including when that triple happens to name the host.
const PORTABLE_CPU: &str = "generic";
const PORTABLE_CPU_FEATURES: &str = "";
const COMPILER_TARGET: &str = env!("LOOM_COMPILER_TARGET");
#[cfg(any(
    test,
    all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")
))]
const WINDOWS_X86_64_DATA_LAYOUT: &str =
    "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128";
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
        trace_llvm_stage("target.identity.begin");
        let data_layout = self.machine.get_target_data().get_data_layout();
        let identity = NativeTargetIdentity {
            triple: self.triple.as_str().to_string_lossy().into_owned(),
            data_layout: data_layout.as_str().to_string_lossy().into_owned(),
            cpu_policy: self.cpu.clone(),
            cpu_features: self.features.clone(),
            optimization: self.optimization.pipeline().to_owned(),
            relocation: RELOCATION_MODE.to_owned(),
        };
        trace_llvm_stage("target.identity.end");
        identity
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
        trace_llvm_stage("target.pointer-width.begin");
        let bits = self
            .machine
            .get_target_data()
            .get_pointer_byte_size(None)
            .checked_mul(8)
            .ok_or_else(|| {
                CodegenError::new(
                    "NativePointerWidthOverflow",
                    format!("target {} pointer width does not fit u32 bits", self.triple),
                )
            })?;
        trace_llvm_stage("target.pointer-width.end");
        Ok(bits)
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
    #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
    {
        // The compiler target and LLVM 19 data layout are build invariants.
        // Runtime packaging needs this identity, not a target machine, so keep
        // the static-MSVC filesystem path independent of LLVM process state.
        Ok(NativeTargetIdentity {
            triple: COMPILER_TARGET.to_owned(),
            data_layout: WINDOWS_X86_64_DATA_LAYOUT.to_owned(),
            cpu_policy: PORTABLE_CPU.to_owned(),
            cpu_features: PORTABLE_CPU_FEATURES.to_owned(),
            optimization: DEVELOPMENT_OPTIMIZATION_PIPELINE.to_owned(),
            relocation: RELOCATION_MODE.to_owned(),
        })
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")))]
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
    trace_llvm_stage("target.normalize.begin");
    let host_native = requested.is_none();
    let triple = TargetMachine::normalize_triple(&TargetTriple::create(
        requested.unwrap_or(COMPILER_TARGET),
    ));
    trace_llvm_stage("target.normalize.end");
    trace_llvm_stage("target.initialize.begin");
    initialize_configured_targets(host_native, &triple)?;
    trace_llvm_stage("target.initialize.end");
    let (cpu, features) = if host_native {
        implicit_host_cpu_policy()
    } else {
        (PORTABLE_CPU.to_owned(), PORTABLE_CPU_FEATURES.to_owned())
    };
    trace_llvm_stage("target.lookup.begin");
    let target = Target::from_triple(&triple)
        .map_err(|message| CodegenError::new("LlvmTargetUnavailable", message.to_string()))?;
    trace_llvm_stage("target.lookup.end");
    trace_llvm_stage("target.machine.begin");
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
    trace_llvm_stage("target.machine.end");
    Ok(NativeTargetMachine {
        triple,
        machine,
        cpu,
        features,
        optimization,
        explicit: requested.is_some(),
    })
}

#[cfg(target_os = "windows")]
fn implicit_host_cpu_policy() -> (String, String) {
    // A generic x86-64 machine is the platform baseline, matches the
    // separately built runtime archive, and gives Windows a deterministic
    // cache identity without an environment-dependent feature probe.
    (PORTABLE_CPU.to_owned(), PORTABLE_CPU_FEATURES.to_owned())
}

#[cfg(not(target_os = "windows"))]
fn implicit_host_cpu_policy() -> (String, String) {
    (
        TargetMachine::get_host_cpu_name().to_string(),
        TargetMachine::get_host_cpu_features().to_string(),
    )
}

#[cfg(loom_llvm_complete_target_set)]
fn initialize_configured_targets(
    host_native: bool,
    _triple: &TargetTriple,
) -> Result<(), CodegenError> {
    let config = InitializationConfig::default();
    if host_native {
        Target::initialize_native(&config)
            .map_err(|message| CodegenError::new("LlvmTargetUnavailable", message))?;
    } else {
        Target::initialize_all(&config);
    }
    Ok(())
}

#[cfg(not(loom_llvm_complete_target_set))]
fn initialize_configured_targets(
    host_native: bool,
    triple: &TargetTriple,
) -> Result<(), CodegenError> {
    let config = InitializationConfig::default();
    if host_native {
        return Target::initialize_native(&config)
            .map_err(|message| CodegenError::new("LlvmTargetUnavailable", message));
    }

    let triple = triple.as_str().to_string_lossy();
    let initialized = if triple_uses_aarch64(&triple) {
        #[cfg(loom_llvm_target_aarch64)]
        Target::initialize_aarch64(&config);
        cfg!(loom_llvm_target_aarch64)
    } else if triple_uses_arm(&triple) {
        #[cfg(loom_llvm_target_arm)]
        Target::initialize_arm(&config);
        cfg!(loom_llvm_target_arm)
    } else if triple_uses_x86(&triple) {
        #[cfg(loom_llvm_target_x86)]
        Target::initialize_x86(&config);
        cfg!(loom_llvm_target_x86)
    } else {
        false
    };
    if initialized {
        Ok(())
    } else {
        Err(CodegenError::new(
            "LlvmTargetUnavailable",
            format!("the configured LLVM target set cannot initialize {triple}"),
        ))
    }
}

#[cfg(not(loom_llvm_complete_target_set))]
fn triple_uses_aarch64(triple: &str) -> bool {
    triple
        .split('-')
        .next()
        .is_some_and(|architecture| matches!(architecture, "aarch64" | "aarch64_be" | "arm64"))
}

#[cfg(not(loom_llvm_complete_target_set))]
fn triple_uses_arm(triple: &str) -> bool {
    triple.split('-').next().is_some_and(|architecture| {
        architecture == "arm"
            || architecture.starts_with("armv")
            || architecture == "thumb"
            || architecture.starts_with("thumbv")
    })
}

#[cfg(not(loom_llvm_complete_target_set))]
fn triple_uses_x86(triple: &str) -> bool {
    triple.split('-').next().is_some_and(|architecture| {
        architecture == "x86_64" || matches!(architecture, "i386" | "i486" | "i586" | "i686")
    })
}

/// Reports whether an explicit triple normalizes to the current host triple.
#[must_use]
pub fn is_native_target(requested: Option<&str>) -> bool {
    let Some(requested) = requested else {
        return true;
    };
    let requested = TargetMachine::normalize_triple(&TargetTriple::create(requested));
    let native = TargetMachine::normalize_triple(&TargetTriple::create(COMPILER_TARGET));
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
    fn implicit_host_identity_uses_the_compiler_target_without_os_version_drift() {
        let expected = TargetMachine::normalize_triple(&TargetTriple::create(COMPILER_TARGET));
        let identity = native_target_identity().expect("native target identity");

        assert_eq!(identity.triple, expected.as_str().to_string_lossy());
    }

    #[cfg(not(loom_llvm_complete_target_set))]
    #[test]
    fn partial_target_selection_is_architecture_exact() {
        assert!(triple_uses_aarch64("aarch64-pc-windows-msvc"));
        assert!(triple_uses_arm("thumbv7em-none-eabihf"));
        assert!(triple_uses_x86("x86_64-pc-windows-msvc"));
        assert!(triple_uses_x86("i686-pc-windows-msvc"));

        for triple in ["riscv64gc-unknown-linux-gnu", "wasm32-unknown-unknown"] {
            assert!(!triple_uses_aarch64(triple));
            assert!(!triple_uses_arm(triple));
            assert!(!triple_uses_x86(triple));
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn static_windows_host_layout_matches_the_pinned_llvm_target() {
        let identity = target_identity(
            Some("x86_64-pc-windows-msvc"),
            OptimizationProfile::Development,
        )
        .expect("pinned LLVM has the Windows x86-64 target");

        assert_eq!(identity.triple, "x86_64-pc-windows-msvc");
        assert_eq!(identity.data_layout, WINDOWS_X86_64_DATA_LAYOUT);
        assert_eq!(identity.cpu_policy, PORTABLE_CPU);
        assert_eq!(identity.cpu_features, PORTABLE_CPU_FEATURES);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn implicit_windows_identity_does_not_probe_unstable_host_cpu_strings() {
        let identity = native_target_identity().expect("Windows host target identity");

        assert_eq!(identity.cpu_policy, PORTABLE_CPU);
        assert_eq!(identity.cpu_features, PORTABLE_CPU_FEATURES);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_llvm_c_api_reports_the_linked_version() {
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        // SAFETY: all three outputs are live, aligned integers owned by this
        // call. This is the smallest state-free LLVM C API linkage probe.
        unsafe { llvm_sys::core::LLVMGetVersion(&raw mut major, &raw mut minor, &raw mut patch) };

        assert_eq!((major, minor, patch), (19, 1, 7));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_llvm_c_api_message_round_trip_is_sound() {
        // SAFETY: the input is a static NUL-terminated string. LLVM owns the
        // returned copy until the matching disposal below.
        let message = unsafe { llvm_sys::core::LLVMCreateMessage(c"loom-llvm-probe".as_ptr()) };
        assert!(!message.is_null());
        // SAFETY: a successful LLVMCreateMessage call returns a valid C string.
        let copied = unsafe { std::ffi::CStr::from_ptr(message) };
        assert_eq!(copied, c"loom-llvm-probe");
        // SAFETY: `message` is the still-owned pointer returned above.
        unsafe { llvm_sys::core::LLVMDisposeMessage(message) };
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_llvm_c_api_normalizes_a_static_triple() {
        // SAFETY: the input is a static NUL-terminated target triple. LLVM
        // returns an owned message which is disposed before this test exits.
        let normalized = unsafe {
            llvm_sys::target_machine::LLVMNormalizeTargetTriple(c"x86_64-pc-windows-msvc".as_ptr())
        };
        assert!(!normalized.is_null());
        // SAFETY: a non-null normalized triple is a valid LLVM-owned C string.
        let normalized_text = unsafe { std::ffi::CStr::from_ptr(normalized) };
        assert_eq!(normalized_text, c"x86_64-pc-windows-msvc");
        // SAFETY: `normalized` is the still-owned pointer returned above.
        unsafe { llvm_sys::core::LLVMDisposeMessage(normalized) };
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_target_machine_constructs_and_reads_layout() {
        let target = create_llvm_target_machine(None, OptimizationProfile::Development)
            .expect("construct native Windows target machine");

        assert_eq!(target.pointer_bits().expect("read pointer width"), 64);
        assert_eq!(target.identity().triple, "x86_64-pc-windows-msvc");
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
