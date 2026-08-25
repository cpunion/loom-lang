//! Native LLVM target-machine policy shared by emission and cache identity.

use inkwell::OptimizationLevel;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::CodegenError;
use crate::emitter::{native_linker_program, native_runtime_bytes};

pub const CPU_POLICY: &str = "generic";
pub const CPU_FEATURES: &str = "";
pub const DEVELOPMENT_OPTIMIZATION_PIPELINE: &str = "default<O0>,globaldce";
pub const RELEASE_OPTIMIZATION_PIPELINE: &str = "default<O2>,globaldce";
pub const RELOCATION_MODE: &str = "pic";
pub const NATIVE_RUNTIME_ABI: &str = loom_runtime_abi::NATIVE_RUNTIME_ABI_IDENTITY;

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
    let (triple, machine) = create_target_machine(triple, optimization)?;
    let data_layout = machine.get_target_data().get_data_layout();
    Ok(NativeTargetIdentity {
        triple: triple.as_str().to_string_lossy().into_owned(),
        data_layout: data_layout.as_str().to_string_lossy().into_owned(),
        cpu_policy: CPU_POLICY.to_owned(),
        cpu_features: CPU_FEATURES.to_owned(),
        optimization: optimization.pipeline().to_owned(),
        relocation: RELOCATION_MODE.to_owned(),
    })
}

/// Returns the selected native linker's version identity without embedding its
/// installation path in cache keys.
///
/// # Errors
///
/// Returns a stable backend error when the selected linker cannot report a
/// successful, non-empty `--version` result.
pub fn native_linker_identity() -> Result<String, CodegenError> {
    let linker = native_linker_program();
    let output = std::process::Command::new(&linker)
        .arg("--version")
        .output()
        .map_err(|error| {
            CodegenError::new(
                "NativeLinkerUnavailable",
                format!("{}: {error}", std::path::Path::new(&linker).display()),
            )
        })?;
    if !output.status.success() {
        return Err(CodegenError::new(
            "NativeLinkerUnavailable",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if version.is_empty() {
        return Err(CodegenError::new(
            "NativeLinkerUnavailable",
            "native linker returned an empty version identity",
        ));
    }
    Ok(version)
}

/// Returns the exact embedded Rust runtime archive identity used by linking.
#[must_use]
pub fn native_runtime_identity() -> String {
    format!(
        "{};sha256={:x}",
        NATIVE_RUNTIME_ABI,
        Sha256::digest(native_runtime_bytes())
    )
}

/// Returns the platform debug-linker's identity when a sidecar is required.
///
/// # Errors
///
/// Returns a stable backend error when macOS `dsymutil` is unavailable.
pub fn native_debug_tool_identity() -> Result<Option<String>, CodegenError> {
    #[cfg(target_os = "macos")]
    {
        let tool = std::env::var_os("LOOM_DSYMUTIL").unwrap_or_else(|| "dsymutil".into());
        let output = std::process::Command::new(&tool)
            .arg("--version")
            .output()
            .map_err(|error| {
                CodegenError::new(
                    "DebugInfoToolUnavailable",
                    format!("{}: {error}", Path::new(&tool).display()),
                )
            })?;
        if !output.status.success() {
            return Err(CodegenError::new(
                "DebugInfoToolUnavailable",
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    Ok(None)
}

/// Returns the standard dSYM DWARF payload path for a native executable.
#[must_use]
pub fn native_debug_companion_path(executable: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let name = executable.file_name()?;
        Some(
            native_debug_bundle_path(executable)
                .join("Contents/Resources/DWARF")
                .join(name),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = executable;
        None
    }
}

/// Links Mach-O object debug sections into a standard dSYM bundle.
///
/// Linux ELF executables already carry their DWARF and this is a no-op there.
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
    #[cfg(not(target_os = "macos"))]
    let _ = executable;
    Ok(())
}

/// Recreates the non-DWARF metadata around a cached dSYM payload.
///
/// # Errors
///
/// Returns a stable backend error if the metadata cannot be written.
pub fn materialize_native_debug_metadata(executable: &Path) -> Result<(), CodegenError> {
    #[cfg(target_os = "macos")]
    {
        let bundle = native_debug_bundle_path(executable);
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(contents.join("Resources/DWARF")).map_err(|error| {
            CodegenError::new(
                "DebugInfoWriteFailed",
                format!("{}: {error}", contents.display()),
            )
        })?;
        let name = executable
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("loom-program");
        let escaped = xml_escape(name);
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\n\
             <key>CFBundleDevelopmentRegion</key><string>English</string>\n\
             <key>CFBundleIdentifier</key><string>org.loom-lang.dsym.{escaped}</string>\n\
             <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>\n\
             <key>CFBundlePackageType</key><string>dSYM</string>\n\
             <key>CFBundleSignature</key><string>????</string>\n\
             <key>CFBundleShortVersionString</key><string>1.0</string>\n\
             <key>CFBundleVersion</key><string>1</string>\n\
             </dict></plist>\n"
        );
        std::fs::write(contents.join("Info.plist"), plist).map_err(|error| {
            CodegenError::new(
                "DebugInfoWriteFailed",
                format!("{}: {error}", contents.display()),
            )
        })?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = executable;
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_debug_bundle_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".dSYM");
    PathBuf::from(name)
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn create_target_machine(
    requested: Option<&str>,
    optimization: OptimizationProfile,
) -> Result<(TargetTriple, TargetMachine), CodegenError> {
    Target::initialize_all(&InitializationConfig::default());
    let triple = requested.map_or_else(TargetMachine::get_default_triple, |triple| {
        TargetMachine::normalize_triple(&TargetTriple::create(triple))
    });
    let target = Target::from_triple(&triple)
        .map_err(|message| CodegenError::new("LlvmTargetUnavailable", message.to_string()))?;
    let machine = target
        .create_target_machine(
            &triple,
            CPU_POLICY,
            CPU_FEATURES,
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
    let pointer_bits = machine
        .get_target_data()
        .get_pointer_byte_size(None)
        .saturating_mul(8);
    if pointer_bits != 64 {
        return Err(CodegenError::new(
            "UnsupportedNativePointerWidth",
            format!(
                "target {triple} uses {pointer_bits}-bit pointers; the current native Value ABI requires 64-bit pointers"
            ),
        ));
    }
    Ok((triple, machine))
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
