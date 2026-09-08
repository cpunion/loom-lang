//! Private backend/platform bridge: accepts checked IR, never Loom source.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use loom_native::codegen::{Backend, EmitOptions, Llvm, Optimization};
use loom_native::native_input;
use loom_native::native_tool::{
    link, output_identity, prepare_parent, publish, reject_source_output,
};

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loom-native: {error}");
            ExitCode::FAILURE
        }
    }
}

enum Action {
    Identity,
    Compile(OsString),
    Link(PathBuf),
}

struct Arguments {
    action: Action,
    output: Option<PathBuf>,
    ir: Option<PathBuf>,
    runtime: Option<PathBuf>,
    linker: Option<PathBuf>,
    protected_directory: Option<PathBuf>,
    expected_identity: Option<String>,
    test_mode: bool,
    object_only: bool,
    library: bool,
    uses_runtime: bool,
}

impl Arguments {
    fn parse(mut arguments: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let input = arguments.next().ok_or("usage: loom-native <checked-input|-> --output <artifact>; --cache-identity [--test]; --link-object <object> --output <artifact>")?;
        let action = if input == "--cache-identity" {
            Action::Identity
        } else if input == "--link-object" {
            Action::Link(PathBuf::from(
                arguments.next().ok_or("--link-object needs an object")?,
            ))
        } else {
            Action::Compile(input)
        };
        let mut parsed = Self {
            action,
            output: None,
            ir: None,
            runtime: None,
            linker: None,
            protected_directory: None,
            expected_identity: None,
            test_mode: false,
            object_only: false,
            library: false,
            uses_runtime: false,
        };
        while let Some(option) = arguments.next() {
            let flag = match option.to_str() {
                Some("--test") => Some(&mut parsed.test_mode),
                Some("--object-only") => Some(&mut parsed.object_only),
                Some("--library") => Some(&mut parsed.library),
                Some("--uses-runtime") => Some(&mut parsed.uses_runtime),
                _ => None,
            };
            if let Some(flag) = flag {
                if *flag {
                    return Err(format!("duplicate {}", option.to_string_lossy()));
                }
                *flag = true;
                continue;
            }
            if option == "--expect-cache-identity" {
                if parsed.expected_identity.is_some() {
                    return Err("duplicate --expect-cache-identity".into());
                }
                parsed.expected_identity = Some(
                    arguments
                        .next()
                        .ok_or("native bridge option needs a value")?
                        .into_string()
                        .map_err(|_| "cache identity must be UTF-8")?,
                );
                continue;
            }
            let target = match option.to_str() {
                Some("--output") => &mut parsed.output,
                Some("--emit-ir") => &mut parsed.ir,
                Some("--runtime") => &mut parsed.runtime,
                Some("--linker") => &mut parsed.linker,
                Some("--protect-directory") => &mut parsed.protected_directory,
                _ => {
                    return Err(format!(
                        "unknown native bridge option {}",
                        option.to_string_lossy()
                    ));
                }
            };
            if target.is_some() {
                return Err(format!("duplicate {}", option.to_string_lossy()));
            }
            *target = Some(PathBuf::from(
                arguments
                    .next()
                    .ok_or("native bridge option needs a value")?,
            ));
        }
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<(), String> {
        match self.action {
            Action::Identity => {
                if self.output.is_some()
                    || self.ir.is_some()
                    || self.runtime.is_some()
                    || self.linker.is_some()
                    || self.protected_directory.is_some()
                    || self.expected_identity.is_some()
                    || self.object_only
                    || self.library
                    || self.uses_runtime
                {
                    return Err("--cache-identity accepts only --test".into());
                }
            }
            Action::Compile(_) => {
                if self.protected_directory.is_some() {
                    return Err("--protect-directory requires --link-object".into());
                }
                if self.library || self.uses_runtime {
                    return Err("--library and --uses-runtime require --link-object".into());
                }
                if self.object_only {
                    if self.ir.is_some() || self.runtime.is_some() || self.linker.is_some() {
                        return Err(
                            "--object-only cannot emit IR or select a runtime/linker".into()
                        );
                    }
                    let identity = self
                        .expected_identity
                        .as_deref()
                        .ok_or("--object-only requires --expect-cache-identity")?;
                    if identity.len() != 64
                        || !identity
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    {
                        return Err("cache identity must be 64 lowercase hexadecimal digits".into());
                    }
                } else if self.expected_identity.is_some() {
                    return Err("--expect-cache-identity requires --object-only".into());
                }
            }
            Action::Link(_) => {
                if self.test_mode
                    || self.object_only
                    || self.expected_identity.is_some()
                    || self.ir.is_some()
                {
                    return Err(
                        "--link-object cannot select tests, code generation, or IR output".into(),
                    );
                }
                if self.library && (self.runtime.is_some() || self.linker.is_some()) {
                    return Err("--link-object --library copies an object and cannot select a runtime/linker".into());
                }
                if self.runtime.is_some() && !self.uses_runtime {
                    return Err("--runtime requires --uses-runtime with --link-object".into());
                }
            }
        }
        if !matches!(self.action, Action::Identity) && self.output.is_none() {
            return Err("native bridge requires --output".into());
        }
        Ok(())
    }
}

fn execute() -> Result<(), String> {
    let arguments = Arguments::parse(std::env::args_os().skip(1))?;
    if let Action::Link(object) = &arguments.action {
        return link_object(object, &arguments);
    }
    let optimization = optimization_level(std::env::var_os("LOOM_OPT_LEVEL").as_deref())?;
    if matches!(arguments.action, Action::Identity) {
        let identity = Llvm.cache_identity(optimization, arguments.test_mode)?;
        println!(
            "loom-native-cache 1\n{}",
            identity.as_deref().unwrap_or("unavailable")
        );
        return Ok(());
    }
    compile(arguments, optimization)
}

fn compile(arguments: Arguments, optimization: Optimization) -> Result<(), String> {
    let Action::Compile(input) = arguments.action else {
        unreachable!()
    };
    let output = arguments.output.unwrap();
    let ir = arguments.ir;
    let runtime = arguments.runtime;
    let linker = arguments.linker;
    let test_mode = arguments.test_mode;
    let mut text = String::new();
    let inputs = if input == "-" {
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("cannot read checked IR: {error}"))?;
        vec![]
    } else {
        let input = PathBuf::from(input);
        text = std::fs::read_to_string(&input)
            .map_err(|error| format!("{}: {error}", input.display()))?;
        vec![input]
    };
    let timings = std::env::var_os("LOOM_NATIVE_TIMINGS").is_some();
    if timings {
        eprintln!("loom-native phase: decode");
    }
    let decode_started = Instant::now();
    let program = native_input::decode(&text)?;
    let decode_time = decode_started.elapsed();
    reject_source_output(&output, &inputs)?;
    prepare_parent(&output)?;
    if let Some(ir) = &ir {
        reject_source_output(ir, &inputs)?;
        prepare_parent(ir)?;
        if output_identity(ir)? == output_identity(&output)? {
            return Err("backend IR and native output paths must differ".into());
        }
    }
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let object = temporary.path().join(if cfg!(windows) {
        "program.obj"
    } else {
        "program.o"
    });
    let emitted_ir = ir.as_ref().map(|_| temporary.path().join("program.ir"));
    if let Some(expected) = &arguments.expected_identity {
        let actual = Llvm.cache_identity(optimization, test_mode)?;
        if actual.as_deref() != Some(expected.as_str()) {
            return Err(
                "native cache identity changed or is unavailable; object not emitted".into(),
            );
        }
    }
    if timings {
        eprintln!("loom-native phase: codegen");
    }
    let backend_started = Instant::now();
    let emission = Llvm.emit(
        &program,
        EmitOptions {
            object: &object,
            ir: emitted_ir.as_deref(),
            test_mode,
            optimization,
        },
    )?;
    let backend_time = backend_started.elapsed();
    if arguments.object_only {
        publish(&object, &output)?;
        println!(
            "loom-native-object 1\n{}\n{}\n{}",
            arguments.expected_identity.unwrap(),
            u8::from(emission.library),
            u8::from(emission.uses_runtime)
        );
        if timings {
            eprintln!(
                "loom-native timings: decode_ms={:.3} codegen_ms={:.3}",
                decode_time.as_secs_f64() * 1000.0,
                backend_time.as_secs_f64() * 1000.0
            );
        }
        return Ok(());
    }
    let executable = temporary.path().join(if cfg!(windows) {
        "program.exe"
    } else {
        "program"
    });
    if timings {
        eprintln!("loom-native phase: link");
    }
    let link_started = Instant::now();
    if !emission.library {
        link(
            &object,
            &executable,
            emission.uses_runtime,
            runtime.as_deref(),
            linker.as_deref().map(|path| path.as_os_str()),
        )?;
    }
    let link_time = link_started.elapsed();
    publish(
        if emission.library {
            &object
        } else {
            &executable
        },
        &output,
    )?;
    if let (Some(from), Some(to)) = (&emitted_ir, &ir) {
        // Once the native file exists, canonicalization also resolves casing
        // aliases on Windows and case-insensitive macOS volumes.
        if output_identity(to)? == output_identity(&output)? {
            return Err("backend IR aliases the native output; native artifact retained".into());
        }
        publish(from, to)?;
    }
    if timings {
        eprintln!(
            "loom-native timings: decode_ms={:.3} codegen_ms={:.3} link_ms={:.3}",
            decode_time.as_secs_f64() * 1000.0,
            backend_time.as_secs_f64() * 1000.0,
            if emission.library {
                0.0
            } else {
                link_time.as_secs_f64() * 1000.0
            }
        );
    }
    Ok(())
}

fn link_object(object: &Path, arguments: &Arguments) -> Result<(), String> {
    let output = arguments.output.as_ref().unwrap();
    let mut sources = vec![object.to_path_buf()];
    sources.extend(arguments.runtime.iter().cloned());
    reject_source_output(output, &sources)?;
    if !object.is_file() {
        return Err("--link-object requires an existing object file".into());
    }
    if let Some(protected) = &arguments.protected_directory {
        let protected = protected
            .canonicalize()
            .map_err(|_| "cannot resolve protected native output directory")?;
        if !protected.is_dir() {
            return Err("--protect-directory requires an existing directory".into());
        }
        // Check the existing ancestor before creating any output directories,
        // including when the final path is nested inside an owned staging tree.
        let absolute = std::path::absolute(output).map_err(|error| error.to_string())?;
        let existing = absolute
            .ancestors()
            .find_map(|path| path.canonicalize().ok())
            .ok_or("cannot resolve native output ancestor")?;
        if existing.starts_with(&protected) {
            return Err("native output cannot be inside the protected directory".into());
        }
        prepare_parent(output)?;
        if output_identity(output)?.starts_with(protected) {
            return Err("native output cannot be inside the protected directory".into());
        }
    } else {
        prepare_parent(output)?;
    }
    let timings = std::env::var_os("LOOM_NATIVE_TIMINGS").is_some();
    if timings {
        eprintln!("loom-native phase: link");
    }
    let started = Instant::now();
    if arguments.library {
        publish(object, output)?;
    } else {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let executable = temporary.path().join(if cfg!(windows) {
            "program.exe"
        } else {
            "program"
        });
        link(
            object,
            &executable,
            arguments.uses_runtime,
            arguments.runtime.as_deref(),
            arguments.linker.as_deref().map(|path| path.as_os_str()),
        )?;
        publish(&executable, output)?;
    }
    if timings {
        eprintln!(
            "loom-native timings: link_ms={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
}

fn optimization_level(value: Option<&OsStr>) -> Result<Optimization, String> {
    match value.unwrap_or(OsStr::new("2")).to_str() {
        Some("0") => Ok(Optimization::O0),
        Some("1") => Ok(Optimization::O1),
        Some("2") => Ok(Optimization::O2),
        Some("3") => Ok(Optimization::O3),
        _ => Err("LOOM_OPT_LEVEL must be 0, 1, 2 or 3".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimization_levels_are_explicit_and_default_to_two() {
        assert_eq!(optimization_level(None).unwrap(), Optimization::O2);
        for (value, expected) in [
            ("0", Optimization::O0),
            ("1", Optimization::O1),
            ("2", Optimization::O2),
            ("3", Optimization::O3),
        ] {
            assert_eq!(
                optimization_level(Some(OsStr::new(value))).unwrap(),
                expected
            );
        }
        for value in ["", "4", "-1", "fast", "02"] {
            assert!(optimization_level(Some(OsStr::new(value))).is_err());
        }
    }

    #[test]
    fn cache_modes_reject_ignored_or_inconsistent_options() {
        fn parse(args: &str) -> Result<Arguments, String> {
            Arguments::parse(args.split_whitespace().map(OsString::from))
        }
        let digest = "0123456789abcdef".repeat(4);
        let object = format!("- --output program.o --object-only --expect-cache-identity {digest}");
        for args in [
            "--cache-identity",
            "--cache-identity --test",
            "- --output program --test --emit-ir program.ll",
            &object,
            "--link-object program.o --output program",
            "--link-object program.o --output program --protect-directory cache",
            "--link-object program.o --output library.o --library --uses-runtime",
            "--link-object program.o --output program --uses-runtime --runtime runtime.a --linker clang",
        ] {
            assert!(parse(args).is_ok(), "{args}");
        }
        for args in [
            "--cache-identity --output program",
            "--cache-identity --test --test",
            "--cache-identity --protect-directory cache",
            "- --output program.o --object-only",
            &format!("- --output program.o --expect-cache-identity {digest}"),
            "- --output program.o --object-only --expect-cache-identity unavailable",
            &format!("{object} --emit-ir program.ll"),
            "- --output program --library",
            "- --output program --protect-directory cache",
            "--link-object program.o --output program --test",
            "--link-object program.o --output program --emit-ir program.ll",
            "--link-object program.o --output program --runtime runtime.a",
            "--link-object program.o --output program --library --linker clang",
        ] {
            assert!(parse(args).is_err(), "{args}");
        }
    }

    #[test]
    fn library_publication_preserves_input_and_rejects_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let object = directory.path().join("input.o");
        let output = directory.path().join("library.o");
        std::fs::write(&object, b"object contents").unwrap();
        let mut arguments = Arguments::parse(
            [
                OsString::from("--link-object"),
                object.clone().into_os_string(),
                OsString::from("--output"),
                output.clone().into_os_string(),
                OsString::from("--library"),
            ]
            .into_iter(),
        )
        .unwrap();
        link_object(&object, &arguments).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"object contents");
        assert_eq!(std::fs::read(&object).unwrap(), b"object contents");
        arguments.output = Some(object.clone());
        assert!(link_object(&object, &arguments).is_err());
        assert_eq!(std::fs::read(&object).unwrap(), b"object contents");

        let protected = directory.path().join("protected");
        std::fs::create_dir(&protected).unwrap();
        arguments.protected_directory = Some(protected.clone());
        arguments.output = Some(output);
        link_object(&object, &arguments).unwrap();
        let nested = protected.join("stage-0/bundle");
        arguments.output = Some(nested.clone());
        assert!(
            link_object(&object, &arguments)
                .unwrap_err()
                .contains("protected directory")
        );
        assert!(!nested.exists());
        arguments.output = Some(protected);
        assert!(
            link_object(&object, &arguments)
                .unwrap_err()
                .contains("protected directory")
        );
    }
}
