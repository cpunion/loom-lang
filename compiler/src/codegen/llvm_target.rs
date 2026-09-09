//! One native target configuration for both emission and cache identity.

use super::{Optimization, OptimizationLevel, configure_codegen};
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(super) struct NativeTarget {
    pub(super) machine: TargetMachine,
    pub(super) optimization: OptimizationLevel,
    relocation: RelocMode,
    code_model: CodeModel,
}

impl NativeTarget {
    pub(super) fn new(optimization: Optimization) -> Result<Self, String> {
        Self::configured(optimization, std::env::var_os("LOOM_TARGET_CPU").as_deref())
    }

    fn configured(optimization: Optimization, cpu: Option<&OsStr>) -> Result<Self, String> {
        let selection = cpu.map_or(Some("native"), OsStr::to_str);
        let (cpu, features) = match selection {
            Some("native") => (
                TargetMachine::get_host_cpu_name().to_string(),
                TargetMachine::get_host_cpu_features().to_string(),
            ),
            Some("generic") => ("generic".into(), String::new()),
            _ => return Err("LOOM_TARGET_CPU must be native or generic".into()),
        };
        configure_codegen();
        Target::initialize_native(&InitializationConfig::default())?;
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).map_err(|error| error.to_string())?;
        let optimization = match optimization {
            Optimization::O0 => OptimizationLevel::None,
            Optimization::O1 => OptimizationLevel::Less,
            Optimization::O2 => OptimizationLevel::Default,
            Optimization::O3 => OptimizationLevel::Aggressive,
        };
        let relocation = if cfg!(windows) {
            RelocMode::Default
        } else {
            RelocMode::PIC
        };
        let code_model = CodeModel::Default;
        let machine = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                optimization,
                relocation,
                code_model,
            )
            .ok_or("LLVM could not create a native target machine")?;
        Ok(Self {
            machine,
            optimization,
            relocation,
            code_model,
        })
    }

    fn profile(&self, test_mode: bool) -> [Vec<u8>; 8] {
        [
            self.machine.get_triple().as_str().to_bytes().to_vec(),
            self.machine.get_cpu().to_bytes().to_vec(),
            self.machine.get_feature_string().to_bytes().to_vec(),
            self.machine
                .get_target_data()
                .get_data_layout()
                .as_str()
                .to_bytes()
                .to_vec(),
            format!("{:?}", self.relocation).into_bytes(),
            format!("{:?}", self.code_model).into_bytes(),
            format!("{:?}", self.optimization).into_bytes(),
            vec![u8::from(test_mode)],
        ]
    }

    pub(super) fn cache_identity(&self, test_mode: bool) -> Option<String> {
        // Toolchain files are trusted and stable during this invocation. Hash
        // their bytes, not versions, paths, mtimes, or an adjacent checksum.
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        let llvm = llvm_module()?.canonicalize().ok()?;
        let native_digest = file_digest(&executable).ok()?;
        let llvm_digest = if llvm == executable {
            native_digest
        } else {
            file_digest(&llvm).ok()?
        };
        Some(identity(
            &native_digest,
            &llvm_digest,
            &self.profile(test_mode),
        ))
    }
}

fn identity(native: &[u8], llvm: &[u8], profile: &[Vec<u8>]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"loom-native-object-backend-v1\0");
    for field in [native, llvm]
        .into_iter()
        .chain(profile.iter().map(Vec::as_slice))
    {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    format!("{:x}", hash.finalize())
}

fn file_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = match file.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&buffer[..length]);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn llvm_module() -> Option<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};

    // SAFETY: the process-linked LLVM remains loaded. dlsym resolves the real
    // dynamic symbol rather than a possible local PLT entry. Non-exported
    // statically linked LLVM falls back to its directly linked code address.
    #[allow(unsafe_code)]
    unsafe {
        let dynamic = libc::dlsym(libc::RTLD_DEFAULT, c"LLVMGetVersion".as_ptr());
        let address = if dynamic.is_null() {
            inkwell::llvm_sys::core::LLVMGetVersion as *const () as *const libc::c_void
        } else {
            dynamic.cast_const()
        };
        let mut info = std::mem::zeroed::<libc::Dl_info>();
        if libc::dladdr(address, &mut info) == 0 || info.dli_fname.is_null() {
            return None;
        }
        let bytes = CStr::from_ptr(info.dli_fname).to_bytes();
        (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

#[cfg(windows)]
fn llvm_module() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };

    let address = inkwell::llvm_sys::core::LLVMGetVersion as *const () as *const u16;
    let mut module = std::ptr::null_mut();
    let mut path = vec![0_u16; 32768];
    // SAFETY: FROM_ADDRESS treats the pointer as linked code, not UTF-16.
    // LLVM stays process-linked, so the borrowed handle cannot be unloaded.
    // llvm-sys currently links LLVM statically on Windows MSVC.
    #[allow(unsafe_code)]
    unsafe {
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address,
            &mut module,
        ) == 0
        {
            return None;
        }
        let length = GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) as usize;
        if length == 0 || length >= path.len() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(
            &path[..length],
        )))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn llvm_module() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_identity_includes_every_profile_field_and_actual_file_bytes() {
        let target = NativeTarget::configured(Optimization::O2, None).unwrap();
        let profile = target.profile(false);
        assert_eq!(
            profile[0],
            TargetMachine::get_default_triple().as_str().to_bytes()
        );
        assert_eq!(profile[1], TargetMachine::get_host_cpu_name().to_bytes());
        assert_eq!(
            profile[2],
            TargetMachine::get_host_cpu_features().to_bytes()
        );
        assert!(!profile[3].is_empty());
        assert_eq!(
            profile[4],
            if cfg!(windows) {
                b"Default".as_slice()
            } else {
                b"PIC"
            }
        );
        assert_eq!(profile[5], b"Default");
        let baseline = identity(b"native", b"llvm", &profile);
        assert_ne!(baseline, identity(b"change", b"llvm", &profile));
        assert_ne!(baseline, identity(b"native", b"edit", &profile));
        for index in 0..profile.len() {
            let mut changed = profile.clone();
            changed[index].push(0);
            assert_ne!(baseline, identity(b"native", b"llvm", &changed));
        }
        assert_ne!(
            baseline,
            identity(b"native", b"llvm", &target.profile(true))
        );
        for optimization in [Optimization::O0, Optimization::O1, Optimization::O3] {
            let different = NativeTarget::configured(optimization, None).unwrap();
            assert_ne!(
                baseline,
                identity(b"native", b"llvm", &different.profile(false))
            );
        }
        assert_ne!(identity(b"a", b"bc", &[]), identity(b"ab", b"c", &[]));

        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("toolchain");
        std::fs::write(&file, b"before").unwrap();
        let before = file_digest(&file).unwrap();
        std::fs::write(&file, b"change").unwrap();
        assert_ne!(before, file_digest(&file).unwrap());
        assert!(file_digest(&temp.path().join("missing")).is_err());
    }

    #[test]
    fn host_llvm_module_has_a_content_identity() {
        let module = llvm_module().expect("loaded LLVM module");
        assert!(module.is_file(), "{}", module.display());
        let identity = NativeTarget::new(Optimization::O2)
            .unwrap()
            .cache_identity(false)
            .expect("readable host LLVM implementation");
        assert_eq!(identity.len(), 64);
        assert!(
            identity
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn generic_cpu_drops_host_features_and_uses_the_effective_cache_profile() {
        let generic =
            NativeTarget::configured(Optimization::O2, Some(OsStr::new("generic"))).unwrap();
        let native =
            NativeTarget::configured(Optimization::O2, Some(OsStr::new("native"))).unwrap();
        let profile = generic.profile(false);
        assert_eq!(profile[1], b"generic");
        assert!(profile[2].is_empty());
        let native_profile = native.profile(false);
        // Host discovery can itself return generic with no features (for
        // example in a VM). Equal effective targets should share a cache key.
        assert_eq!(
            identity(b"native", b"llvm", &profile) == identity(b"native", b"llvm", &native_profile),
            profile == native_profile
        );
        assert!(NativeTarget::configured(Optimization::O2, Some(OsStr::new("typo"))).is_err());
    }
}
