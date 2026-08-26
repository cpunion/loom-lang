use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

const REPORT_KIND: &str = "loom-cross-language-basic-benchmark";
const REPORT_WARNING: &str = "Controlled microbenchmark evidence, not a general language ranking.";
const DEFAULT_WARMUPS: usize = 3;
const DEFAULT_RUNS: usize = 10;
const THROUGHPUT_WARMUPS: usize = 2;
const THROUGHPUT_RUNS: usize = 5;
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
const STANDARD_TIMEOUT: Duration = Duration::from_secs(30);
const THROUGHPUT_TIMEOUT: Duration = Duration::from_secs(60);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone, Copy)]
struct CaseSpec {
    name: &'static str,
    description: &'static str,
    standard_scale: i64,
    throughput_scale: i64,
    quick_scale: i64,
    checksum: fn(i64) -> Result<i64, String>,
}

const CASES: &[CaseSpec] = &[
    CaseSpec {
        name: "int_lcg",
        description: "bounded Int arithmetic and a counted loop",
        standard_scale: 2_000_000,
        throughput_scale: 100_000_000,
        quick_scale: 10_000,
        checksum: lcg_final_checksum,
    },
    CaseSpec {
        name: "record_method",
        description: "mutable record method calls with a bounded periodic value",
        standard_scale: 500_000,
        throughput_scale: 100_000_000,
        quick_scale: 10_000,
        checksum: list_checksum,
    },
    CaseSpec {
        name: "list_build_scan",
        description: "grow an Int list and scan it by index",
        standard_scale: 10_000,
        throughput_scale: 10_000_000,
        quick_scale: 1_000,
        checksum: list_checksum,
    },
    CaseSpec {
        name: "fib_recursive",
        description: "non-tail recursive calls over Int",
        standard_scale: 32,
        throughput_scale: 40,
        quick_scale: 20,
        checksum: fib_checksum,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkProfile {
    Standard,
    Quick,
    Throughput,
}

impl BenchmarkProfile {
    const fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Quick => "quick",
            Self::Throughput => "throughput",
        }
    }

    const fn scale(self, case: CaseSpec) -> i64 {
        match self {
            Self::Standard => case.standard_scale,
            Self::Quick => case.quick_scale,
            Self::Throughput => case.throughput_scale,
        }
    }

    const fn default_warmups(self) -> usize {
        match self {
            Self::Standard => DEFAULT_WARMUPS,
            Self::Quick => 1,
            Self::Throughput => THROUGHPUT_WARMUPS,
        }
    }

    const fn default_runs(self) -> usize {
        match self {
            Self::Standard => DEFAULT_RUNS,
            Self::Quick => 3,
            Self::Throughput => THROUGHPUT_RUNS,
        }
    }

    const fn default_timeout(self) -> Duration {
        match self {
            Self::Standard => STANDARD_TIMEOUT,
            Self::Quick => QUICK_TIMEOUT,
            Self::Throughput => THROUGHPUT_TIMEOUT,
        }
    }
}

#[derive(Debug)]
struct Config {
    profile: BenchmarkProfile,
    allow_busy_host: bool,
    warmups: usize,
    runs: usize,
    invocation_timeout: Duration,
    output: Option<PathBuf>,
    selected_cases: Vec<String>,
}

#[derive(Clone)]
struct ToolCommand {
    program: OsString,
    prefix_args: Vec<OsString>,
}

impl ToolCommand {
    fn from_env(name: &str, default: impl Into<OsString>) -> Self {
        Self {
            program: std::env::var_os(name).unwrap_or_else(|| default.into()),
            prefix_args: Vec::new(),
        }
    }

    fn rustc() -> Self {
        if let Some(program) = std::env::var_os("LOOM_BENCH_RUSTC") {
            return Self {
                program,
                prefix_args: Vec::new(),
            };
        }
        Self {
            program: OsString::from("rustup"),
            prefix_args: vec![
                OsString::from("run"),
                OsString::from("1.88.0"),
                OsString::from("rustc"),
            ],
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args);
        command
    }

    fn display_argv(&self, args: &[OsString]) -> Vec<String> {
        std::iter::once(&self.program)
            .chain(self.prefix_args.iter())
            .chain(args.iter())
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }
}

#[derive(Clone)]
struct LanguageSpec {
    language: &'static str,
    source: PathBuf,
    executable: PathBuf,
    compiler: ToolCommand,
    version_args: Vec<OsString>,
    compile_args: Vec<OsString>,
    runtime_environment: Vec<(&'static str, &'static str)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u32,
    kind: &'static str,
    status: &'static str,
    warning: &'static str,
    generated_at_unix_ms: u128,
    host: HostReport,
    config: ConfigReport,
    toolchains: Vec<ToolchainReport>,
    cases: Vec<CaseReport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostReport {
    os: &'static str,
    architecture: &'static str,
    cpu: String,
    logical_cpus: usize,
    load_average_1m_before_build: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigReport {
    profile: &'static str,
    busy_host_override: bool,
    warmups: usize,
    measured_runs: usize,
    invocation_timeout_ms: u64,
    optimization_policy: &'static str,
    execution_order: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolchainReport {
    language: &'static str,
    version: String,
    compile_argv: Vec<String>,
    compile_ms: f64,
    binary_bytes: u64,
    source_sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    name: &'static str,
    description: &'static str,
    scale: i64,
    expected_checksum: i64,
    results: Vec<RuntimeReport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeReport {
    language: &'static str,
    samples_ns: Vec<u64>,
    minimum_ms: f64,
    p05_ms: f64,
    median_ms: f64,
    mean_ms: f64,
    p95_ms: f64,
    maximum_ms: f64,
    relative_to_fastest_median: f64,
}

#[derive(Clone, Copy)]
struct Summary {
    minimum: f64,
    p05: f64,
    median: f64,
    mean: f64,
    p95: f64,
    maximum: f64,
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) => {
            eprintln!("loom-benchmark: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let config = parse_config(std::env::args_os().skip(1))?;
    let workspace = workspace_root();
    let cases = select_cases(&config)?;
    let languages = language_specs(&workspace)?;
    let host = host_report();
    reject_busy_measured_run(&config, &host)?;
    let mut toolchains = Vec::with_capacity(languages.len());

    eprintln!("building {} benchmark executables", languages.len());
    for language in &languages {
        toolchains.push(build_language(language, &workspace)?);
    }

    let mut case_reports = Vec::with_capacity(cases.len());
    for (case_index, case) in cases.iter().enumerate() {
        let scale = config.profile.scale(*case);
        let expected = (case.checksum)(scale)?;
        eprintln!(
            "measuring {} (scale {scale}, {} warmups + {} runs)",
            case.name, config.warmups, config.runs
        );
        case_reports.push(measure_case(
            case,
            scale,
            expected,
            &languages,
            case_index,
            config.warmups,
            config.runs,
            config.invocation_timeout,
            &workspace,
        )?);
    }

    let generated_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system time precedes Unix epoch: {error}"))?
        .as_millis();
    let report = Report {
        schema_version: 1,
        kind: REPORT_KIND,
        status: "passed",
        warning: REPORT_WARNING,
        generated_at_unix_ms,
        host,
        config: ConfigReport {
            profile: config.profile.name(),
            busy_host_override: config.allow_busy_host,
            warmups: config.warmups,
            measured_runs: config.runs,
            invocation_timeout_ms: duration_millis(config.invocation_timeout),
            optimization_policy: "native release/O2, no cross-language LTO assumption",
            execution_order: "rotated by case and round to reduce fixed-order bias",
        },
        toolchains,
        cases: case_reports,
    };
    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("serialize benchmark report: {error}"))?;
    if let Some(output) = config.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        fs::write(&output, format!("{json}\n"))
            .map_err(|error| format!("write {}: {error}", output.display()))?;
        eprintln!("wrote {}", output.display());
    }
    println!("{json}");
    Ok(())
}

fn parse_config(arguments: impl IntoIterator<Item = OsString>) -> Result<Config, String> {
    let mut profile = BenchmarkProfile::Standard;
    let mut allow_busy_host = false;
    let mut warmups = None;
    let mut runs = None;
    let mut invocation_timeout = None;
    let mut output = None;
    let mut selected_cases = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--quick") => select_profile(&mut profile, BenchmarkProfile::Quick, "--quick")?,
            Some("--throughput") => {
                select_profile(&mut profile, BenchmarkProfile::Throughput, "--throughput")?;
            }
            Some("--allow-busy-host") => allow_busy_host = true,
            Some("--warmups") => {
                warmups = Some(parse_count("--warmups", arguments.next())?);
            }
            Some("--runs") => runs = Some(parse_count("--runs", arguments.next())?),
            Some("--timeout-seconds") => {
                invocation_timeout = Some(parse_timeout(arguments.next())?);
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--output requires a path".to_owned())?,
                ));
            }
            Some("--case") => {
                selected_cases.push(
                    arguments
                        .next()
                        .ok_or_else(|| "--case requires a name".to_owned())?
                        .into_string()
                        .map_err(|_| "--case must be valid UTF-8".to_owned())?,
                );
            }
            Some("--help" | "-h") => {
                print_help();
                std::process::exit(0);
            }
            Some(value) => return Err(format!("unknown argument `{value}`; use --help")),
            None => return Err("arguments must be valid UTF-8".to_owned()),
        }
    }
    let warmups = warmups.unwrap_or(profile.default_warmups());
    let runs = runs.unwrap_or(profile.default_runs());
    let invocation_timeout = invocation_timeout.unwrap_or(profile.default_timeout());
    if runs == 0 {
        return Err("--runs must be greater than zero".to_owned());
    }
    Ok(Config {
        profile,
        allow_busy_host,
        warmups,
        runs,
        invocation_timeout,
        output,
        selected_cases,
    })
}

fn parse_timeout(value: Option<OsString>) -> Result<Duration, String> {
    let seconds = parse_count("--timeout-seconds", value)?;
    if seconds == 0 {
        return Err("--timeout-seconds must be greater than zero".to_owned());
    }
    let seconds = u64::try_from(seconds)
        .map_err(|_| "--timeout-seconds is too large for this platform".to_owned())?;
    Ok(Duration::from_secs(seconds))
}

fn select_profile(
    selected: &mut BenchmarkProfile,
    requested: BenchmarkProfile,
    flag: &str,
) -> Result<(), String> {
    if *selected != BenchmarkProfile::Standard {
        return Err(format!(
            "benchmark profiles are mutually exclusive; cannot combine {flag} with --{}",
            selected.name()
        ));
    }
    *selected = requested;
    Ok(())
}

fn parse_count(flag: &str, value: Option<OsString>) -> Result<usize, String> {
    let value = value.ok_or_else(|| format!("{flag} requires a value"))?;
    let value = value
        .to_str()
        .ok_or_else(|| format!("{flag} must be valid UTF-8"))?;
    value
        .parse::<usize>()
        .map_err(|error| format!("invalid {flag} value `{value}`: {error}"))
}

fn print_help() {
    println!(
        "usage: loom-benchmark [--quick | --throughput] [--allow-busy-host]\n\
         \x20                      [--warmups N] [--runs N] [--timeout-seconds N]\n\
         \x20                      [--case NAME] [--output FILE]\n\
         \n\
         Builds Loom, Go, Rust, C, and C++ fixtures once, validates every checksum,\n\
         and emits one machine-readable JSON report. Repeat --case to select cases.\n\
         --quick is a correctness smoke; --throughput uses amplified dynamic scales."
    );
}

fn select_cases(config: &Config) -> Result<Vec<CaseSpec>, String> {
    if config.selected_cases.is_empty() {
        return Ok(CASES.to_vec());
    }
    let mut selected = Vec::with_capacity(config.selected_cases.len());
    for requested in &config.selected_cases {
        let case = CASES
            .iter()
            .find(|case| case.name == requested)
            .copied()
            .ok_or_else(|| {
                let names = CASES
                    .iter()
                    .map(|case| case.name)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown case `{requested}`; expected one of: {names}")
            })?;
        selected.push(case);
    }
    Ok(selected)
}

fn language_specs(workspace: &Path) -> Result<Vec<LanguageSpec>, String> {
    let source_dir = workspace.join("benchmarks/basic/programs");
    let output_dir = workspace.join("target/benchmarks/basic/bin");
    fs::create_dir_all(&output_dir)
        .map_err(|error| format!("create {}: {error}", output_dir.display()))?;
    let loomc = std::env::var_os("LOOM_BENCH_LOOMC").map_or_else(
        || executable_path(&workspace.join("target/release"), "loomc").into_os_string(),
        |path| path,
    );
    let go = ToolCommand::from_env("LOOM_BENCH_GO", "go");
    let rustc = ToolCommand::rustc();
    let cc = ToolCommand::from_env("LOOM_BENCH_CC", "clang");
    let cxx = ToolCommand::from_env("LOOM_BENCH_CXX", "clang++");

    Ok(vec![
        loom_spec(&source_dir, &output_dir, loomc),
        go_spec(&source_dir, &output_dir, go),
        rust_spec(&source_dir, &output_dir, rustc),
        c_spec(&source_dir, &output_dir, cc),
        cpp_spec(&source_dir, &output_dir, cxx),
    ])
}

fn executable_path(directory: &Path, stem: &str) -> PathBuf {
    directory.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

fn loom_spec(source_dir: &Path, output_dir: &Path, program: OsString) -> LanguageSpec {
    let executable = executable_path(output_dir, "loom-basic");
    LanguageSpec {
        language: "loom",
        source: source_dir.join("main.loom"),
        executable: executable.clone(),
        compiler: ToolCommand {
            program,
            prefix_args: Vec::new(),
        },
        version_args: vec![OsString::from("--version")],
        compile_args: vec![
            OsString::from("--release"),
            OsString::from("--no-cache"),
            OsString::from("build"),
            OsString::from("--output"),
            executable.into_os_string(),
            source_dir.as_os_str().to_owned(),
        ],
        runtime_environment: Vec::new(),
    }
}

fn go_spec(source_dir: &Path, output_dir: &Path, compiler: ToolCommand) -> LanguageSpec {
    let executable = executable_path(output_dir, "go-basic");
    LanguageSpec {
        language: "go",
        source: source_dir.join("main.go"),
        executable: executable.clone(),
        compiler,
        version_args: vec![OsString::from("version")],
        compile_args: vec![
            OsString::from("build"),
            OsString::from("-trimpath"),
            OsString::from("-ldflags=-s -w"),
            OsString::from("-o"),
            executable.into_os_string(),
            source_dir.join("main.go").into_os_string(),
        ],
        runtime_environment: vec![("GOMAXPROCS", "1")],
    }
}

fn rust_spec(source_dir: &Path, output_dir: &Path, compiler: ToolCommand) -> LanguageSpec {
    let executable = executable_path(output_dir, "rust-basic");
    LanguageSpec {
        language: "rust",
        source: source_dir.join("main.rs"),
        executable: executable.clone(),
        compiler,
        version_args: vec![OsString::from("--version")],
        compile_args: vec![
            OsString::from("--edition=2024"),
            OsString::from("-C"),
            OsString::from("opt-level=2"),
            OsString::from("-C"),
            OsString::from("overflow-checks=yes"),
            OsString::from("-C"),
            OsString::from("codegen-units=1"),
            OsString::from("-C"),
            OsString::from("debuginfo=0"),
            OsString::from("-D"),
            OsString::from("warnings"),
            OsString::from("-o"),
            executable.into_os_string(),
            source_dir.join("main.rs").into_os_string(),
        ],
        runtime_environment: Vec::new(),
    }
}

fn c_spec(source_dir: &Path, output_dir: &Path, compiler: ToolCommand) -> LanguageSpec {
    let executable = executable_path(output_dir, "c-basic");
    LanguageSpec {
        language: "c",
        source: source_dir.join("main.c"),
        executable: executable.clone(),
        compiler,
        version_args: vec![OsString::from("--version")],
        compile_args: vec![
            OsString::from("-O2"),
            OsString::from("-DNDEBUG"),
            OsString::from("-std=c17"),
            OsString::from("-Wall"),
            OsString::from("-Wextra"),
            OsString::from("-Werror"),
            OsString::from("-o"),
            executable.into_os_string(),
            source_dir.join("main.c").into_os_string(),
        ],
        runtime_environment: Vec::new(),
    }
}

fn cpp_spec(source_dir: &Path, output_dir: &Path, compiler: ToolCommand) -> LanguageSpec {
    let executable = executable_path(output_dir, "cpp-basic");
    LanguageSpec {
        language: "cpp",
        source: source_dir.join("main.cpp"),
        executable: executable.clone(),
        compiler,
        version_args: vec![OsString::from("--version")],
        compile_args: vec![
            OsString::from("-O2"),
            OsString::from("-DNDEBUG"),
            OsString::from("-std=c++20"),
            OsString::from("-Wall"),
            OsString::from("-Wextra"),
            OsString::from("-Werror"),
            OsString::from("-o"),
            executable.into_os_string(),
            source_dir.join("main.cpp").into_os_string(),
        ],
        runtime_environment: Vec::new(),
    }
}

fn build_language(language: &LanguageSpec, workspace: &Path) -> Result<ToolchainReport, String> {
    if !language.source.is_file() {
        return Err(format!(
            "missing benchmark source {}",
            language.source.display()
        ));
    }
    let version = command_output(
        &language.compiler,
        &language.version_args,
        workspace,
        &[],
        "query compiler version",
    )?;
    let started = Instant::now();
    let output = execute_command(&language.compiler, &language.compile_args, workspace, &[])
        .map_err(|error| format!("compile {} benchmark: {error}", language.language))?;
    let elapsed = started.elapsed();
    require_success(output, &format!("compile {} benchmark", language.language))?;
    let binary_bytes = fs::metadata(&language.executable)
        .map_err(|error| format!("inspect {}: {error}", language.executable.display()))?
        .len();
    let source = fs::read(&language.source)
        .map_err(|error| format!("read {}: {error}", language.source.display()))?;
    eprintln!(
        "  {:>4}: {:>8.2} ms, {} bytes",
        language.language,
        milliseconds(elapsed),
        binary_bytes
    );
    Ok(ToolchainReport {
        language: language.language,
        version: version.trim().to_owned(),
        compile_argv: language.compiler.display_argv(&language.compile_args),
        compile_ms: milliseconds(elapsed),
        binary_bytes,
        source_sha256: format!("{:x}", Sha256::digest(source)),
    })
}

#[allow(clippy::too_many_arguments)]
fn measure_case(
    case: &CaseSpec,
    scale: i64,
    expected: i64,
    languages: &[LanguageSpec],
    case_index: usize,
    warmups: usize,
    runs: usize,
    invocation_timeout: Duration,
    workspace: &Path,
) -> Result<CaseReport, String> {
    let mut samples = languages
        .iter()
        .map(|language| (language.language, Vec::with_capacity(runs)))
        .collect::<BTreeMap<_, _>>();
    for round in 0..warmups {
        for language_index in rotated_indices(languages.len(), case_index + round) {
            run_fixture(
                &languages[language_index],
                case.name,
                scale,
                expected,
                invocation_timeout,
                workspace,
            )?;
        }
    }
    for round in 0..runs {
        for language_index in rotated_indices(languages.len(), case_index + round) {
            let language = &languages[language_index];
            let duration = run_fixture(
                language,
                case.name,
                scale,
                expected,
                invocation_timeout,
                workspace,
            )?;
            samples
                .get_mut(language.language)
                .expect("all language sample buckets exist")
                .push(nanos(duration));
        }
    }

    let summaries = languages
        .iter()
        .map(|language| {
            let language_samples = samples
                .get(language.language)
                .expect("all language sample buckets exist");
            (language.language, summarize(language_samples))
        })
        .collect::<BTreeMap<_, _>>();
    let fastest_median = summaries
        .values()
        .map(|summary| summary.median)
        .fold(f64::INFINITY, f64::min);
    let results = languages
        .iter()
        .map(|language| {
            let summary = summaries
                .get(language.language)
                .expect("all language summaries exist");
            RuntimeReport {
                language: language.language,
                samples_ns: samples
                    .remove(language.language)
                    .expect("all language sample buckets exist"),
                minimum_ms: summary.minimum,
                p05_ms: summary.p05,
                median_ms: summary.median,
                mean_ms: summary.mean,
                p95_ms: summary.p95,
                maximum_ms: summary.maximum,
                relative_to_fastest_median: summary.median / fastest_median,
            }
        })
        .collect();
    Ok(CaseReport {
        name: case.name,
        description: case.description,
        scale,
        expected_checksum: expected,
        results,
    })
}

fn run_fixture(
    language: &LanguageSpec,
    case: &str,
    scale: i64,
    expected: i64,
    timeout: Duration,
    workspace: &Path,
) -> Result<Duration, String> {
    let scale = scale.to_string();
    let expected = expected.to_string();
    let started = Instant::now();
    let action = format!("execute {} {case}", language.language);
    let mut command = Command::new(&language.executable);
    command
        .args([case, &scale, &expected])
        .envs(language.runtime_environment.iter().copied())
        .env("LC_ALL", "C")
        .current_dir(workspace);
    let output = output_with_timeout(&mut command, timeout, &action)?;
    let elapsed = started.elapsed();
    require_success(output, &action).and_then(|stdout| {
        if matches!(stdout.as_bytes(), b"Unit\n" | b"Unit\r\n") {
            Ok(elapsed)
        } else {
            Err(format!(
                "execute {} {case}: expected one platform line `Unit`, got {stdout:?}",
                language.language
            ))
        }
    })
}

fn rotated_indices(length: usize, rotation: usize) -> impl Iterator<Item = usize> {
    (0..length).map(move |offset| (rotation + offset) % length)
}

fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    action: &str,
) -> Result<Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{action}: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("collect {action} output: {error}"));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("poll {action}: {error}"));
            }
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let kill_error = child.kill().err();
            let output = child
                .wait_with_output()
                .map_err(|error| format!("collect timed-out {action} output: {error}"))?;
            let kill_note = kill_error.map_or_else(
                || "process killed and reaped".to_owned(),
                |error| format!("kill reported {error}"),
            );
            return Err(format!(
                "{action} timed out after {} ms ({kill_note})\nstdout:\n{}\nstderr:\n{}",
                duration_millis(timeout),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        std::thread::sleep(CHILD_POLL_INTERVAL.min(timeout - elapsed));
    }
}

fn execute_command(
    tool: &ToolCommand,
    args: &[OsString],
    current_dir: &Path,
    environment: &[(&str, &str)],
) -> std::io::Result<Output> {
    tool.command()
        .args(args)
        .envs(environment.iter().copied())
        .current_dir(current_dir)
        .output()
}

fn command_output(
    tool: &ToolCommand,
    args: &[OsString],
    current_dir: &Path,
    environment: &[(&str, &str)],
    action: &str,
) -> Result<String, String> {
    let output = execute_command(tool, args, current_dir, environment)
        .map_err(|error| format!("{action}: {error}"))?;
    require_success(output, action)
}

fn require_success(output: Output, action: &str) -> Result<String, String> {
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map_err(|error| format!("{action}: stdout is not UTF-8: {error}"));
    }
    Err(format!(
        "{action} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn first_nonempty_line(value: &str) -> String {
    value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("unknown")
        .trim()
        .to_owned()
}

fn summarize(samples_ns: &[u64]) -> Summary {
    assert!(!samples_ns.is_empty(), "at least one sample is required");
    let mut sorted = samples_ns.to_vec();
    sorted.sort_unstable();
    let sum = sorted
        .iter()
        .map(|sample| u128::from(*sample))
        .sum::<u128>();
    let mean_ns = sum / u128::try_from(sorted.len()).expect("sample count fits in u128");
    Summary {
        minimum: ns_to_ms(sorted[0]),
        p05: ns_to_ms(nearest_rank(&sorted, 1, 20)),
        median: ns_to_ms(nearest_rank(&sorted, 1, 2)),
        mean: ns_to_ms(u64::try_from(mean_ns).unwrap_or(u64::MAX)),
        p95: ns_to_ms(nearest_rank(&sorted, 19, 20)),
        maximum: ns_to_ms(*sorted.last().expect("samples are non-empty")),
    }
}

fn nearest_rank(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let span = sorted.len() - 1;
    let index = (span * numerator + denominator / 2) / denominator;
    sorted[index]
}

fn lcg_final_checksum(iterations: i64) -> Result<i64, String> {
    validate_scale(iterations)?;
    let modulus = 2_147_483_647_i128;
    let mut exponent = iterations;
    let mut base = 48_271_i128;
    let mut result = 1_i128;
    for _ in 0..63 {
        let half = exponent / 2;
        if exponent - half * 2 == 1 {
            result = (result * base) % modulus;
        }
        base = (base * base) % modulus;
        exponent = half;
    }
    i64::try_from(result).map_err(|_| format!("checksum exceeds Int for scale {iterations}"))
}

fn list_checksum(length: i64) -> Result<i64, String> {
    validate_scale(length)?;
    let full_blocks = i128::from(length / 1_024);
    let remainder = i128::from(length - (length / 1_024) * 1_024);
    let full_block_sum = 1_023_i128 * 1_024 / 2;
    let sum = full_blocks * full_block_sum + remainder * (remainder - 1) / 2;
    i64::try_from(sum).map_err(|_| format!("checksum exceeds Int for scale {length}"))
}

fn fib_checksum(depth: i64) -> Result<i64, String> {
    validate_scale(depth)?;
    let mut previous = 0_i64;
    let mut current = 1_i64;
    for _ in 0..depth {
        let next = previous
            .checked_add(current)
            .ok_or_else(|| format!("Fibonacci checksum exceeds Int for depth {depth}"))?;
        previous = current;
        current = next;
    }
    Ok(previous)
}

fn validate_scale(scale: i64) -> Result<(), String> {
    if scale < 0 {
        return Err(format!("benchmark scale must be non-negative, got {scale}"));
    }
    Ok(())
}

fn host_report() -> HostReport {
    HostReport {
        os: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        cpu: cpu_name(),
        logical_cpus: std::thread::available_parallelism().map_or(1, usize::from),
        load_average_1m_before_build: load_average_1m(),
    }
}

fn reject_busy_measured_run(config: &Config, host: &HostReport) -> Result<(), String> {
    if config.profile == BenchmarkProfile::Quick || config.allow_busy_host {
        return Ok(());
    }
    let Some(load) = host.load_average_1m_before_build else {
        return Ok(());
    };
    let logical_cpus = u32::try_from(host.logical_cpus).unwrap_or(u32::MAX);
    let threshold = f64::from(logical_cpus).mul_add(0.75, 0.0).max(1.0);
    if load > threshold {
        return Err(format!(
            "host is too busy for a controlled measurement: 1-minute load average {load:.2} exceeds {threshold:.2}; wait for the machine to become idle or pass --allow-busy-host to record an explicitly noisy run"
        ));
    }
    Ok(())
}

fn load_average_1m() -> Option<f64> {
    if cfg!(target_os = "macos") {
        let output = Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()?;
        if output.status.success() {
            return parse_first_number(&String::from_utf8_lossy(&output.stdout));
        }
    }
    if cfg!(target_os = "linux") {
        return fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|value| parse_first_number(&value));
    }
    None
}

fn parse_first_number(value: &str) -> Option<f64> {
    value.split_whitespace().find_map(|piece| {
        piece
            .trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
            .parse::<f64>()
            .ok()
    })
}

fn cpu_name() -> String {
    if cfg!(target_os = "macos") {
        if let Ok(output) = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
        {
            if output.status.success() {
                return first_nonempty_line(&String::from_utf8_lossy(&output.stdout));
            }
        }
    }
    if cfg!(target_os = "linux") {
        if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
            if let Some(name) = cpuinfo.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(_, value)| value.trim().to_owned())
            }) {
                return name;
            }
        }
    }
    "unknown".to_owned()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("benchmark crate is inside the workspace")
        .to_path_buf()
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[allow(clippy::cast_precision_loss)]
fn ns_to_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{
        BenchmarkProfile, CASES, CaseReport, ConfigReport, HostReport, REPORT_KIND, REPORT_WARNING,
        Report, RuntimeReport, executable_path, fib_checksum, lcg_final_checksum, list_checksum,
        nearest_rank, output_with_timeout, parse_config, parse_first_number,
        reject_busy_measured_run, summarize,
    };

    #[test]
    fn executable_paths_use_the_host_suffix() {
        assert_eq!(
            executable_path(std::path::Path::new("bin"), "fixture"),
            std::path::Path::new("bin").join(format!("fixture{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn standard_checksums_are_stable() {
        assert_eq!(lcg_final_checksum(2_000_000), Ok(24_123_260));
        assert_eq!(list_checksum(500_000), Ok(255_644_016));
        assert_eq!(list_checksum(10_000), Ok(5_020_920));
        assert_eq!(fib_checksum(32), Ok(2_178_309));
    }

    #[test]
    fn throughput_profile_uses_amplified_dynamic_scales() {
        let config = parse_config([OsString::from("--throughput")]).expect("throughput config");
        assert_eq!(config.profile, BenchmarkProfile::Throughput);
        assert_eq!(config.warmups, 2);
        assert_eq!(config.runs, 5);
        assert_eq!(
            CASES
                .iter()
                .map(|case| config.profile.scale(*case))
                .collect::<Vec<_>>(),
            [100_000_000, 100_000_000, 10_000_000, 40]
        );
        for (case, expected) in
            CASES
                .iter()
                .zip([373_370_831, 51_149_901_696, 5_114_877_120, 102_334_155])
        {
            assert_eq!((case.checksum)(config.profile.scale(*case)), Ok(expected));
        }
    }

    #[test]
    fn benchmark_profiles_are_mutually_exclusive() {
        for flags in [
            ["--quick", "--throughput"],
            ["--throughput", "--quick"],
            ["--quick", "--quick"],
            ["--throughput", "--throughput"],
        ] {
            let error =
                parse_config(flags.map(OsString::from)).expect_err("profile combination must fail");
            assert!(error.contains("mutually exclusive"), "{error}");
        }
    }

    #[test]
    fn busy_host_guard_covers_standard_and_throughput_only() {
        let busy = HostReport {
            os: "test",
            architecture: "test",
            cpu: "test".into(),
            logical_cpus: 4,
            load_average_1m_before_build: Some(3.01),
        };
        let idle = HostReport {
            load_average_1m_before_build: Some(3.0),
            ..HostReport {
                os: "test",
                architecture: "test",
                cpu: "test".into(),
                logical_cpus: 4,
                load_average_1m_before_build: None,
            }
        };
        let standard = parse_config([]).expect("standard config");
        let throughput = parse_config([OsString::from("--throughput")]).expect("throughput config");
        let quick = parse_config([OsString::from("--quick")]).expect("quick config");
        let override_config = parse_config([
            OsString::from("--throughput"),
            OsString::from("--allow-busy-host"),
        ])
        .expect("busy override config");

        assert!(reject_busy_measured_run(&standard, &busy).is_err());
        assert!(reject_busy_measured_run(&throughput, &busy).is_err());
        assert!(reject_busy_measured_run(&quick, &busy).is_ok());
        assert!(reject_busy_measured_run(&override_config, &busy).is_ok());
        assert!(reject_busy_measured_run(&standard, &idle).is_ok());
    }

    #[test]
    fn timeout_override_is_positive_and_profile_defaults_are_stable() {
        assert_eq!(
            parse_config([])
                .expect("standard config")
                .invocation_timeout,
            Duration::from_secs(30)
        );
        assert_eq!(
            parse_config([OsString::from("--quick")])
                .expect("quick config")
                .invocation_timeout,
            Duration::from_secs(10)
        );
        assert_eq!(
            parse_config([OsString::from("--throughput")])
                .expect("throughput config")
                .invocation_timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            parse_config([
                OsString::from("--throughput"),
                OsString::from("--timeout-seconds"),
                OsString::from("7"),
            ])
            .expect("timeout override")
            .invocation_timeout,
            Duration::from_secs(7)
        );
        assert!(parse_config([OsString::from("--timeout-seconds"), OsString::from("0"),]).is_err());
    }

    #[test]
    fn report_json_records_profile_scale_checksum_timeout_and_raw_samples() {
        let report = Report {
            schema_version: 1,
            kind: REPORT_KIND,
            status: "passed",
            warning: REPORT_WARNING,
            generated_at_unix_ms: 0,
            host: HostReport {
                os: "test",
                architecture: "test",
                cpu: "test".into(),
                logical_cpus: 4,
                load_average_1m_before_build: Some(1.0),
            },
            config: ConfigReport {
                profile: "throughput",
                busy_host_override: false,
                warmups: 2,
                measured_runs: 5,
                invocation_timeout_ms: 60_000,
                optimization_policy: "test",
                execution_order: "test",
            },
            toolchains: Vec::new(),
            cases: vec![CaseReport {
                name: "list_build_scan",
                description: "test",
                scale: 10_000_000,
                expected_checksum: 5_114_877_120,
                results: vec![RuntimeReport {
                    language: "loom",
                    samples_ns: vec![10, 11],
                    minimum_ms: 0.000_010,
                    p05_ms: 0.000_010,
                    median_ms: 0.000_010,
                    mean_ms: 0.000_010,
                    p95_ms: 0.000_011,
                    maximum_ms: 0.000_011,
                    relative_to_fastest_median: 1.0,
                }],
            }],
        };
        let json = serde_json::to_value(report).expect("serialize report");
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["config"]["profile"], "throughput");
        assert_eq!(json["config"]["invocationTimeoutMs"], 60_000);
        assert_eq!(json["cases"][0]["scale"], 10_000_000);
        assert_eq!(json["cases"][0]["expectedChecksum"], 5_114_877_120_i64);
        assert_eq!(
            json["cases"][0]["results"][0]["samplesNs"],
            serde_json::json!([10, 11])
        );
    }

    #[test]
    fn timeout_child() {
        if std::env::var_os("LOOM_BENCH_TIMEOUT_CHILD").is_some() {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn timed_out_children_are_killed_and_reaped() {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args(["--exact", "tests::timeout_child"])
            .env("LOOM_BENCH_TIMEOUT_CHILD", "1");
        let started = Instant::now();
        let error = output_with_timeout(
            &mut command,
            Duration::from_millis(20),
            "timeout test child",
        )
        .expect_err("child must time out");
        assert!(error.contains("timed out after 20 ms"), "{error}");
        assert!(error.contains("process killed and reaped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn summary_uses_sorted_nearest_rank_samples() {
        let samples = [5_000_000, 1_000_000, 3_000_000, 2_000_000, 4_000_000];
        let summary = summarize(&samples);
        assert!((summary.minimum - 1.0).abs() < f64::EPSILON);
        assert!((summary.median - 3.0).abs() < f64::EPSILON);
        assert!((summary.maximum - 5.0).abs() < f64::EPSILON);
        assert!((summary.mean - 3.0).abs() < f64::EPSILON);
        let mut sorted = samples;
        sorted.sort_unstable();
        assert_eq!(nearest_rank(&sorted, 19, 20), 5_000_000);
    }

    #[test]
    fn parses_macos_and_linux_load_average_formats() {
        assert_eq!(parse_first_number("{ 12.34 5.67 1.23 }"), Some(12.34));
        assert_eq!(parse_first_number("0.42 0.30 0.10 1/123 456"), Some(0.42));
    }
}
