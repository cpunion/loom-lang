//! Private backend/platform bridge: accepts checked IR, never Loom source.

use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;
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

fn execute() -> Result<(), String> {
    let optimization = optimization_level(std::env::var_os("LOOM_OPT_LEVEL").as_deref())?;
    let mut arguments = std::env::args_os().skip(1);
    let input = arguments.next().ok_or("usage: loom-native <checked-input|-> --output <artifact> [--test] [--emit-ir <path>] [--runtime <archive>] [--linker <driver>]")?;
    let mut output = None;
    let mut ir = None;
    let mut runtime = None;
    let mut linker = None;
    let mut test_mode = false;
    while let Some(option) = arguments.next() {
        if option == "--test" {
            if test_mode {
                return Err("duplicate --test".into());
            }
            test_mode = true;
        } else {
            let target = match option.to_str() {
                Some("--output") => &mut output,
                Some("--emit-ir") => &mut ir,
                Some("--runtime") => &mut runtime,
                Some("--linker") => &mut linker,
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
    }
    let output = output.ok_or("native bridge requires --output")?;
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
}
