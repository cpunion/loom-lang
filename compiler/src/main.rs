use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use loom_seed::{check, native, package};

fn main() -> ExitCode {
    match execute() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("loom: {message}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<ExitCode, String> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        help();
        return Ok(ExitCode::SUCCESS);
    };
    if command == "--help" || command == "-h" {
        help();
        return Ok(ExitCode::SUCCESS);
    }
    if command == "--version" {
        println!("loom native seed {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    let command = command.to_str().ok_or("command must be UTF-8")?;
    if !matches!(command, "check" | "build" | "test" | "run") {
        return Err(format!("unknown command {command}; use --help"));
    }
    let mut path = None;
    let mut output = None;
    let mut ir = None;
    while let Some(arg) = args.next() {
        if arg == "--output" || arg == "-o" {
            if output.is_some() {
                return Err("duplicate --output".into());
            }
            output = Some(PathBuf::from(args.next().ok_or("--output needs a path")?));
        } else if arg == "--emit-ir" {
            if ir.is_some() {
                return Err("duplicate --emit-ir".into());
            }
            ir = Some(PathBuf::from(args.next().ok_or("--emit-ir needs a path")?));
        } else if arg.to_string_lossy().starts_with('-') {
            return Err(format!(
                "unknown option {}; use --help",
                arg.to_string_lossy()
            ));
        } else if path.replace(PathBuf::from(arg)).is_some() {
            return Err("select one package directory".into());
        }
    }
    if command != "build" && output.is_some() {
        return Err("--output is only available with build".into());
    }
    if command == "check" && ir.is_some() {
        return Err("check does not emit LLVM IR".into());
    }
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let loaded = package::load(&path, command == "test")?;
    let program = check::check(&loaded.files, &loaded.root_package, command == "test")
        .map_err(|error| package::diagnostic(&error, &loaded.sources))?;
    if command == "check" {
        println!("checked {} package source files", loaded.files.len());
        return Ok(ExitCode::SUCCESS);
    }
    if command == "run" && program.entry.is_none() {
        return Err("run requires fn main() in the selected package".into());
    }
    if command == "test" && program.tests.is_empty() {
        println!("0 tests");
        return Ok(ExitCode::SUCCESS);
    }
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let object = temporary.path().join("program.o");
    let library = command == "build" && program.entry.is_none();
    let artifact = if command == "build" {
        output.unwrap_or_else(|| {
            path.join("target")
                .join(if library { "package.o" } else { "main" })
        })
    } else {
        temporary.path().join("program")
    };
    reject_source_output(&artifact, &loaded.sources)?;
    prepare_parent(&artifact)?;
    if let Some(ir) = &ir {
        reject_source_output(ir, &loaded.sources)?;
        prepare_parent(ir)?;
        if output_identity(ir)? == output_identity(&artifact)? {
            return Err("LLVM IR and native output paths must differ".into());
        }
    }
    let temporary_ir = ir.as_ref().map(|_| temporary.path().join("program.ll"));
    let uses_runtime = native::emit(
        &program,
        command == "test",
        &object,
        temporary_ir.as_deref(),
    )?;
    let executable = temporary.path().join("program");
    if command == "build" {
        if !library {
            link(&object, &executable, uses_runtime)?;
        }
        publish(if library { &object } else { &executable }, &artifact)?;
    } else {
        link(&object, &artifact, uses_runtime)?;
    }
    if let (Some(from), Some(to)) = (&temporary_ir, &ir) {
        publish(from, to)?;
    }
    if command == "build" {
        println!("built {}", artifact.display());
        return Ok(ExitCode::SUCCESS);
    }
    let status = Command::new(&artifact)
        .status()
        .map_err(|error| format!("cannot run {}: {error}", artifact.display()))?;
    if !status.success() {
        return Err(format!("native program failed ({status})"));
    }
    if command == "test" {
        println!("{} tests passed", program.tests.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn help() {
    println!(
        "Loom native seed\n\n  loom check [directory]\n  loom build [directory] [--output path] [--emit-ir path]\n  loom test  [directory] [--emit-ir path]\n  loom run   [directory] [--emit-ir path]\n\nThe seed implements a documented scalar subset. build emits a library object\nwhen the selected package has no main. LOOM_CC selects the host Clang linker."
    );
}

fn link(object: &Path, output: &Path, uses_runtime: bool) -> Result<(), String> {
    let linker = std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into());
    let mut command = Command::new(&linker);
    command.arg(object);
    if uses_runtime {
        let runtime = std::env::var_os("LOOM_RUNTIME_LIBRARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap_or_default()
                    .with_file_name("libloom_seed_runtime.a")
            });
        if !runtime.is_file() {
            return Err(format!(
                "missing native runtime {}: run cargo build --workspace or set LOOM_RUNTIME_LIBRARY",
                runtime.display()
            ));
        }
        command.arg(runtime);
        if cfg!(target_os = "linux") {
            command.args(["-ldl", "-lpthread", "-lm"]);
        }
    }
    let result = command
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|error| format!("cannot start host linker: {error}"))?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "host linker failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ))
    }
}

fn prepare_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    Ok(())
}

fn output_identity(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let parent = absolute.parent().ok_or("output needs a parent directory")?;
    Ok(parent
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(absolute.file_name().ok_or("output needs a file name")?))
}

fn publish(from: &Path, to: &Path) -> Result<(), String> {
    // Replace a directory entry only after successful emission/linking. Copying
    // directly into a user-selected path would also modify hard-linked inputs.
    let destination = output_identity(to)?;
    let staging = tempfile::NamedTempFile::new_in(destination.parent().unwrap())
        .map_err(|error| error.to_string())?;
    std::fs::copy(from, staging.path()).map_err(|error| error.to_string())?;
    staging
        .persist(destination)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn reject_source_output(path: &Path, sources: &[loom_seed::model::Source]) -> Result<(), String> {
    // Catch source/manifest overwrites and existing aliases before invoking LLVM/linker.
    if path.is_symlink() {
        return Err("native output paths cannot be symbolic links".into());
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "loom" || extension == "toml" || extension == "lock")
    {
        return Err("native outputs cannot overwrite source, manifests, or lockfiles".into());
    }
    if let Ok(canonical) = path.canonicalize() {
        if sources
            .iter()
            .any(|source| source.path.canonicalize().ok().as_ref() == Some(&canonical))
        {
            return Err("native output aliases a source file".into());
        }
    }
    Ok(())
}
