//! Private LLVM/platform bridge: accepts checked IR, never Loom source.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use loom_seed::native_tool::{
    link, output_identity, prepare_parent, publish, reject_source_output,
};
use loom_seed::{native, native_input};

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
    let mut arguments = std::env::args_os().skip(1);
    let input = arguments.next().ok_or("usage: loom-native <checked-input|-> --output <artifact> [--test] [--emit-ir <path>] [--runtime <archive>] [--linker <clang>]")?;
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
    let program = native_input::decode(&text)?;
    reject_source_output(&output, &inputs)?;
    prepare_parent(&output)?;
    if let Some(ir) = &ir {
        reject_source_output(ir, &inputs)?;
        prepare_parent(ir)?;
        if output_identity(ir)? == output_identity(&output)? {
            return Err("LLVM IR and native output paths must differ".into());
        }
    }
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let object = temporary.path().join("program.o");
    let emitted_ir = ir.as_ref().map(|_| temporary.path().join("program.ll"));
    let uses_runtime = native::emit(&program, test_mode, &object, emitted_ir.as_deref())?;
    let executable = temporary.path().join("program");
    let library = !test_mode && program.entry.is_none();
    if !library {
        link(
            &object,
            &executable,
            uses_runtime,
            runtime.as_deref(),
            linker.as_deref().map(|path| path.as_os_str()),
        )?;
    }
    publish(if library { &object } else { &executable }, &output)?;
    if let (Some(from), Some(to)) = (&emitted_ir, &ir) {
        publish(from, to)?;
    }
    Ok(())
}
