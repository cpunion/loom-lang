use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use loom_core::{FileId, Span};
use loom_driver::{
    AnalysisHost, CacheContext, CacheLookup, DiagnosticRecord, LIBRARY_ARTIFACT_MAX_BYTES,
    LIBRARY_ARTIFACT_VERSION, LockMode, PersistentCache, PipelineStage, Position, ProjectGraph,
    ProjectOptions, RelatedDiagnostic, SourceOrigin, SpanRecord, SymbolId, TargetKind,
    decode_library_artifact, discover_loom_files, encode_library_artifact, format_source,
};
use loom_hir::{SourceUnit, lower_files};
use loom_interpreter::{Interpreter, TestStatus, Value};
use loom_mir::ConceptIdentity;
use loom_sema::{CallTarget, TaskIntrinsic};
use loom_syntax::parse_with_file;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn new() -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("loom-driver-test-{}-{serial}", std::process::id()));
        fs::create_dir_all(&root).expect("create test project");
        Self { root }
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("test path has parent"))
            .expect("create source parent");
        fs::write(path, text).expect("write source");
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn loom_text_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[test]
fn loom_text_literals_escape_windows_paths_before_source_interpolation() {
    assert_eq!(
        loom_text_literal("C:\\loom\\round-trip.txt"),
        "C:\\\\loom\\\\round-trip.txt"
    );
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("discovered file is below root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn portable_cache_context() -> CacheContext {
    CacheContext {
        language_version: "0.3".to_owned(),
        frontend_identity: "test-frontend-v1".to_owned(),
        stdlib_identity: "test-stdlib-v1".to_owned(),
        contract_mode: "checked".to_owned(),
    }
}

fn constrained_proof_project() -> TestProject {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"proofs\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "src/main.loom",
        "module proofs\n\ntype Positive = Float where self >= 0.0\n\npub fn make() Positive { Positive(10.0) }\n",
    );
    project
}

fn assert_compiler_std_source(source: &loom_driver::SourceDocument) {
    assert_eq!(source.origin(), SourceOrigin::CompilerStd);
    assert!(source.is_compiler_std());
    assert!(!source.is_embedded_dependency());
    assert!(source.is_read_only());
    assert!(!source.is_navigable());
}

fn assert_compiler_owned_overlays_are_ignored(
    host: &mut AnalysisHost,
    int_path: &Path,
    json_path: &Path,
    log_path: &Path,
    process_path: &Path,
    resource_path: &Path,
) {
    host.set_overlay(
        int_path,
        "module std.int\n\npub fn minimum(left Int, right Int) Int { 999 }\n",
    )
    .expect("install hostile synthetic-path overlay");
    host.set_overlay(
        json_path,
        "module std.json\n\npub fn forged_json() Int { 999 }\n",
    )
    .expect("install hostile std.json overlay");
    host.set_overlay(
        log_path,
        "module std.log\n\npub fn debug(message Text) { discard message }\n",
    )
    .expect("install hostile std.log overlay");
    host.set_overlay(
        process_path,
        "module std.process\n\npub fn arguments() List[Text] { [\"forged\"] }\n",
    )
    .expect("install hostile std.process overlay");
    host.set_overlay(
        resource_path,
        "module std.resource\n\npub concept ForgedResourceProtocol {}\n",
    )
    .expect("install hostile std.resource overlay");

    let protected = host.snapshot().expect("reload protected std source");
    for (path, expected, message) in [
        (int_path, "Returns the smaller", "std.int source"),
        (json_path, "fn finish_utf8", "std.json source"),
        (log_path, "Writes one debug-level", "std.log source"),
        (
            process_path,
            "Returns the arguments passed",
            "std.process source",
        ),
        (
            resource_path,
            "Requires a value to transfer directly",
            "std.resource protocols",
        ),
    ] {
        let source = protected
            .sources()
            .documents()
            .iter()
            .find(|source| source.absolute_path() == path)
            .unwrap_or_else(|| panic!("missing protected {message}"));
        assert!(
            source.text().is_some_and(|text| text.contains(expected)),
            "compiler-owned {message} must win over editor overlays"
        );
    }
    assert!(!protected.has_errors(), "{:#?}", protected.diagnostics());
}

fn project_using_source_backed_std_functions() -> TestProject {
    let project = TestProject::new();
    project.write(
        "main.loom",
        r#"module embedded_std_user

import std.int.minimum
import std.log.debug

fn emit() {
    debug("not executed")
}

pub fn main() {
    let value = minimum(9, 4)
    assert value == 4
}

test fn importedMinimum() {
    let value = minimum(-2, 3)
    assert value == -2
}
"#,
    );
    project
}

#[test]
fn compiler_std_sources_have_protected_authority() {
    let project = project_using_source_backed_std_functions();
    let mut host = AnalysisHost::new(&project.root).expect("open embedded-std project");
    let snapshot = host.snapshot().expect("check embedded-std project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());

    let root_source = snapshot
        .sources()
        .documents()
        .iter()
        .find(|source| source.is_root_package())
        .expect("root source");
    assert_eq!(root_source.origin(), SourceOrigin::FileSystem);
    assert!(root_source.is_navigable());
    assert!(!root_source.is_read_only());

    let std_sources = snapshot
        .sources()
        .documents()
        .iter()
        .filter(|source| {
            source
                .package()
                .is_some_and(|package| package.name() == "std")
        })
        .collect::<Vec<_>>();
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/json.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/log.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/process.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/resource.loom")),
        "{std_sources:#?}"
    );
    for source in &std_sources {
        assert_compiler_std_source(source);
    }
    let resource_path = std_sources
        .iter()
        .find(|source| source.relative_path().ends_with("src/resource.loom"))
        .expect("compiler-owned std.resource source")
        .absolute_path()
        .to_path_buf();
    let json_path = std_sources
        .iter()
        .find(|source| source.relative_path().ends_with("src/json.loom"))
        .expect("compiler-owned std.json source")
        .absolute_path()
        .to_path_buf();
    let log_path = std_sources
        .iter()
        .find(|source| source.relative_path().ends_with("src/log.loom"))
        .expect("compiler-owned std.log source")
        .absolute_path()
        .to_path_buf();
    let process_path = std_sources
        .iter()
        .find(|source| source.relative_path().ends_with("src/process.loom"))
        .expect("compiler-owned std.process source")
        .absolute_path()
        .to_path_buf();
    let std_source = std_sources
        .iter()
        .copied()
        .find(|source| source.relative_path().ends_with("src/int.loom"))
        .expect("compiler-owned std.int source");
    assert_compiler_std_source(std_source);
    assert_eq!(
        std_source.package().expect("std package").version(),
        loom_core::LOOM_LANGUAGE_VERSION
    );
    assert_eq!(std_source.relative_path(), "deps/std@0.3/src/int.loom");

    let std_path = std_source.absolute_path().to_path_buf();
    drop(snapshot);
    assert_compiler_owned_overlays_are_ignored(
        &mut host,
        &std_path,
        &json_path,
        &log_path,
        &process_path,
        &resource_path,
    );
}

#[test]
fn source_backed_std_functions_lower_to_direct_calls_and_run() {
    let project = project_using_source_backed_std_functions();
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open source-backed std project")
        .snapshot()
        .expect("check source-backed std project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("lower checked MIR");
    let minimum = program
        .functions
        .iter()
        .find(|function| function.name == "std.int.minimum")
        .expect("ordinary source minimum definition");
    assert!(
        program.functions.iter().any(|function| {
            function.exprs_preorder().any(|expression| {
                matches!(
                    expression.kind,
                    loom_mir::ExprKind::Call {
                        target: loom_mir::CallTarget::Direct(target),
                        ..
                    } if target == minimum.id
                )
            })
        }),
        "the imported source function must lower to a direct DefId-derived call"
    );
    let debug = program
        .functions
        .iter()
        .find(|function| function.name == "std.log.debug")
        .expect("ordinary source debug definition");
    let emit = program
        .functions
        .iter()
        .find(|function| function.name == "embedded_std_user.emit")
        .expect("root caller of source-backed debug");
    assert!(
        emit.exprs_preorder().any(|expression| {
            matches!(
                expression.kind,
                loom_mir::ExprKind::Call {
                    target: loom_mir::CallTarget::Direct(target),
                    ..
                } if target == debug.id
            )
        }),
        "std.log.debug must resolve through its ordinary source DefId"
    );

    let results = snapshot.run_tests().expect("run imported std function");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed);
}

#[test]
fn std_package_name_alias_and_complete_namespace_are_reserved() {
    let named_std = TestProject::new();
    named_std.write(
        "loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"std\"\nversion = \"1.0.0\"\n",
    );
    named_std.write("src/main.loom", "module std\n\npub fn main() {}\n");
    let error = ProjectGraph::load(&named_std.root).expect_err("reserved package name");
    assert!(error.to_string().contains("package name `std` is reserved"));

    let aliased_std = TestProject::new();
    aliased_std.write(
        "loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\nstd = { path = \"../dependency\" }\n",
    );
    aliased_std.write("src/main.loom", "module application\n\npub fn main() {}\n");
    let error = ProjectGraph::load(&aliased_std.root).expect_err("reserved dependency alias");
    assert!(
        error
            .to_string()
            .contains("dependency alias `std` is reserved")
    );

    for module in [
        "std",
        "std.int",
        "std.json",
        "std.process",
        "std.resource",
        "std.future.nested",
    ] {
        let standalone_namespace = TestProject::new();
        standalone_namespace.write(
            "main.loom",
            &format!("module {module}\n\npub fn main() {{}}\n"),
        );
        let snapshot = AnalysisHost::new(&standalone_namespace.root)
            .expect("open standalone namespace project")
            .snapshot()
            .expect("analyze standalone namespace project");
        assert!(
            snapshot
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "ReservedStdModule"),
            "{module}: {:#?}",
            snapshot.diagnostics()
        );
    }

    let adjacent_namespace = TestProject::new();
    adjacent_namespace.write("main.loom", "module stdish.resource\n\npub fn main() {}\n");
    let snapshot = AnalysisHost::new(&adjacent_namespace.root)
        .expect("open adjacent namespace project")
        .snapshot()
        .expect("analyze adjacent namespace project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
}

#[test]
fn human_diagnostics_use_scalar_columns_and_keep_machine_records_stable() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module demo\n\npub fn main() {\n    let 价格 = 1\n}\n",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open source")
        .snapshot()
        .expect("load source map");
    let span = |start_line, start_column, end_line, end_column| SpanRecord {
        path: "main.loom".to_owned(),
        start_byte: 0,
        end_byte: 0,
        start_line,
        start_column,
        end_line,
        end_column,
    };
    let record = DiagnosticRecord {
        schema_version: 1,
        category: "diagnostic".to_owned(),
        severity: "error".to_owned(),
        code: "Example".to_owned(),
        message: "example failure".to_owned(),
        primary_span: span(4, 9, 4, 11),
        related: vec![
            RelatedDiagnostic {
                label: "first relation".to_owned(),
                span: span(1, 1, 1, 7),
            },
            RelatedDiagnostic {
                label: "second relation".to_owned(),
                span: span(5, 5, 5, 9),
            },
        ],
        notes: vec!["first note".to_owned(), "second note".to_owned()],
        details: std::collections::BTreeMap::default(),
    };
    let json_before = serde_json::to_value(&record).expect("serialize stable JSON record");
    let human = record.human_with_source(snapshot.sources());
    assert!(human.contains("4 |     let 价格 = 1"), "{human}");
    assert!(human.contains("  |         ^^"), "{human}");
    let first_relation = human.find("first relation").expect("first relation");
    let second_relation = human.find("second relation").expect("second relation");
    let first_note = human.find("first note").expect("first note");
    let second_note = human.find("second note").expect("second note");
    assert!(first_relation < second_relation);
    assert!(second_relation < first_note);
    assert!(first_note < second_note);
    assert_eq!(
        serde_json::to_value(&record).expect("serialize unchanged JSON record"),
        json_before
    );

    let multiline = DiagnosticRecord {
        primary_span: span(4, 9, 5, 5),
        related: Vec::new(),
        notes: Vec::new(),
        ..record
    }
    .human_with_source(snapshot.sources());
    assert!(multiline.contains("  |         ^^^^^^"), "{multiline}");
    assert!(!multiline.contains("    Unit"), "{multiline}");
}

#[test]
fn cache_inventory_and_prune_are_versioned_and_bounded() {
    let project = TestProject::new();
    let cache = PersistentCache::new(project.root.join("cache/v2"));
    let key = PersistentCache::semantic_key("cache-test", &[("input", "one")]);
    cache
        .store_artifact(&key, b"reachable")
        .expect("store reachable cache blob");
    let orphan_digest = "a".repeat(64);
    project.write(
        &format!("cache/v2/blobs/sha256/aa/{orphan_digest}"),
        "orphan",
    );
    project.write("cache/v2/refs/broken/not-json.json", "not json");

    let stats = cache.stats().expect("inventory cache");
    assert_eq!(stats.schema_version, loom_driver::CACHE_SCHEMA_VERSION);
    assert_eq!(stats.references, 2);
    assert_eq!(stats.invalid_references, 1);
    assert_eq!(stats.blobs, 2);
    assert_eq!(stats.reclaimable_blobs, 1);

    let report = cache.prune().expect("prune cache");
    assert_eq!(report.invalid_references_removed, 1);
    assert_eq!(report.blobs_removed, 1);
    assert_eq!(report.bytes_reclaimed, 6);
    let after = cache.stats().expect("inventory pruned cache");
    assert_eq!(after.invalid_references, 0);
    assert_eq!(after.reclaimable_blobs, 0);
}

#[test]
fn first_class_dynamic_values_execute_in_the_interpreter() {
    let project = TestProject::new();
    project.write(
        "concepts.loom",
        include_str!("../../../examples/concepts-polymorphism/concepts.loom"),
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open dynamic-value project")
        .snapshot()
        .expect("compile dynamic-value project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute dynamic-value tests");
    assert!(
        results
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{results:#?}"
    );
    assert!(results.iter().any(|result| {
        result
            .name
            .ends_with("dynamic_interfaces_are_first_class_values")
    }));
}

#[test]
fn async_dynamic_values_execute_in_the_interpreter() {
    let project = TestProject::new();
    project.write(
        "tasks.loom",
        include_str!("../../../examples/async-resources/tasks.loom"),
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open async dynamic-value project")
        .snapshot()
        .expect("compile async dynamic-value project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot
        .run_tests()
        .expect("execute async dynamic-value tests");
    assert!(
        results
            .iter()
            .all(|result| result.status == TestStatus::Passed),
        "{results:#?}"
    );
}

#[test]
fn discovery_is_recursive_sorted_and_ignores_git_and_target() {
    let project = TestProject::new();
    project.write("z.loom", "module z\n");
    project.write("src/a.loom", "module a\n");
    project.write("target/generated.loom", "module ignored\n");
    project.write("nested/.git/hidden.loom", "module ignored\n");
    project.write("src/not-loom.txt", "module ignored\n");

    let files = discover_loom_files(&project.root).expect("discover project");
    let canonical_root = fs::canonicalize(&project.root).expect("canonical test root");
    let relative = files
        .iter()
        .map(|path| relative(&canonical_root, path))
        .collect::<Vec<_>>();
    assert_eq!(relative, ["src/a.loom", "z.loom"]);
}

#[test]
fn manifest_resolves_path_dependencies_sources_and_targets() {
    let project = TestProject::new();
    project.write(
        "utility/loom.toml",
        "schema = 1\n\n[package]\nname = \"utility\"\nversion = \"1.2.0\"\nsources = [\"src\"]\n",
    );
    project.write(
        "utility/src/math.loom",
        "module utility.math\n\npub fn increment(value Int) Int {\n    value + 1\n}\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 1\n\n[package]\nname = \"application\"\nversion = \"0.1.0\"\nsources = [\"src\"]\n\n[dependencies]\nutility = { path = \"../utility\", version = \"^1.0\" }\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.start\"\n\n[[target]]\nname = \"unit\"\nkind = \"test\"\n\n[[target]]\nname = \"api\"\nkind = \"lib\"\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.math.increment\n\npub fn start() {\n    let value = increment(1)\n    assert value == 2\n}\n\ntest fn dependency_works() {\n    let value = increment(4)\n    assert value == 5\n}\n",
    );

    let host = AnalysisHost::new(project.root.join("application")).expect("open manifest project");
    assert!(host.project().manifest().is_some());
    assert_eq!(host.project().packages().count(), 2);
    let target = host.project().target("app").expect("binary target");
    assert_eq!(target.kind(), TargetKind::Bin);
    assert_eq!(target.entry(), Some("application.start"));
    let library = host.project().target("api").expect("library target");
    assert_eq!(library.kind(), TargetKind::Lib);
    assert_eq!(library.entry(), None);

    let snapshot = host.snapshot().expect("compile package graph");
    let paths = snapshot
        .sources()
        .documents()
        .iter()
        .filter(|source| !source.is_compiler_std())
        .map(loom_driver::SourceDocument::relative_path)
        .collect::<Vec<_>>();
    assert_eq!(paths, ["deps/utility@1.2.0/src/math.loom", "src/main.loom"]);
    let std_paths = snapshot
        .sources()
        .documents()
        .iter()
        .filter(|source| source.is_compiler_std())
        .map(loom_driver::SourceDocument::relative_path)
        .collect::<Vec<_>>();
    for expected in [
        "deps/std@0.3/src/int.loom",
        "deps/std@0.3/src/json.loom",
        "deps/std@0.3/src/log.loom",
        "deps/std@0.3/src/process.loom",
        "deps/std@0.3/src/resource.loom",
    ] {
        assert!(std_paths.contains(&expected), "{std_paths:?}");
    }
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    assert!(
        snapshot
            .executable()
            .expect("manifest graph lowers to MIR")
            .exports
            .contains_key("application.start")
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_library_is_a_consumable_versioned_dependency() {
    let project = TestProject::new();
    project.write(
        "utility/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"utility\"\nversion = \"1.2.0\"\nsources = [\"src\", \"deps\"]\n",
    );
    project.write(
        "utility/src/math.loom",
        "module utility.math\n\npub fn increment(value Int) Int { value + 1 }\n\nfn private_value() Int { 99 }\n",
    );
    project.write(
        "utility/deps/utility@1.2.0/nested.loom",
        "module utility.nested\n\npub fn nested_value() Int { 7 }\n",
    );
    let producer = AnalysisHost::new(project.root.join("utility")).expect("open producer");
    let producer_snapshot = producer.snapshot().expect("compile producer");
    assert!(
        !producer_snapshot.has_errors(),
        "{:#?}",
        producer_snapshot.diagnostics()
    );
    let bytes = encode_library_artifact(producer_snapshot.project(), producer_snapshot.sources())
        .expect("encode package artifact");
    let envelope: serde_json::Value = serde_json::from_slice(&bytes).expect("library JSON");
    assert!(
        envelope["sources"]
            .as_array()
            .expect("library sources")
            .iter()
            .all(|source| source["package"]["name"] != "std"),
        "compiler-owned std sources must not be serialized into a portable package"
    );
    let mut reserved_package = envelope.clone();
    reserved_package["packages"][0]["id"]["name"] = serde_json::json!("std");
    let error = decode_library_artifact(
        &serde_json::to_vec(&reserved_package).expect("encode reserved package artifact"),
    )
    .expect_err("the public decoder rejects the compiler-owned package name");
    assert!(
        error.to_string().contains("reserved package `std`"),
        "{error}"
    );

    let mut reserved_alias = envelope.clone();
    let dependency_package = reserved_alias["packages"][0]["id"].clone();
    reserved_alias["packages"][0]["dependencies"] = serde_json::json!([{
        "alias": "std",
        "requirement": null,
        "package": dependency_package,
    }]);
    let error = decode_library_artifact(
        &serde_json::to_vec(&reserved_alias).expect("encode reserved alias artifact"),
    )
    .expect_err("the public decoder rejects the compiler-owned dependency alias");
    assert!(
        error
            .to_string()
            .contains("reserved dependency alias `std`"),
        "{error}"
    );

    let previous = LIBRARY_ARTIFACT_VERSION
        .checked_sub(1)
        .expect("library artifact version must be positive");
    let next = LIBRARY_ARTIFACT_VERSION
        .checked_add(1)
        .expect("library artifact version must fit u32");
    for unsupported in [previous, next] {
        let mut mismatched_version = envelope.clone();
        mismatched_version["version"] = serde_json::json!(unsupported);
        mismatched_version["producerState"] = serde_json::json!("must not be decoded");
        let error = decode_library_artifact(
            &serde_json::to_vec(&mismatched_version).expect("encode mismatched artifact"),
        )
        .expect_err("a mismatched artifact version must be rejected before its body");
        assert_eq!(error.code(), "LibraryArtifactVersionMismatch");
        assert!(
            error.to_string().contains(&unsupported.to_string()),
            "{error}"
        );
    }

    let mut unexpected_field = envelope.clone();
    unexpected_field["producerState"] = serde_json::json!("private implementation detail");
    let error = decode_library_artifact(
        &serde_json::to_vec(&unexpected_field).expect("encode artifact with an unknown field"),
    )
    .expect_err("the current artifact rejects unknown producer state");
    assert_eq!(error.code(), "InvalidLibraryArtifact");

    let mut nested_extra = envelope.clone();
    nested_extra["rootPackage"]["producerState"] = serde_json::json!(true);
    let error = decode_library_artifact(
        &serde_json::to_vec(&nested_extra).expect("encode nested package extension"),
    )
    .expect_err("package identities reject unknown wire fields");
    assert_eq!(error.code(), "InvalidLibraryArtifact");
    let mut nested_extra = envelope.clone();
    nested_extra["publicInterfaces"][0]["producerState"] = serde_json::json!(true);
    let error = decode_library_artifact(
        &serde_json::to_vec(&nested_extra).expect("encode nested interface extension"),
    )
    .expect_err("public interfaces reject unknown wire fields");
    assert_eq!(error.code(), "InvalidLibraryArtifact");

    let mut drive_relative_source = envelope.clone();
    drive_relative_source["sources"][0]["path"] = serde_json::json!("C:escape.loom");
    let error = decode_library_artifact(
        &serde_json::to_vec(&drive_relative_source).expect("encode drive-relative source"),
    )
    .expect_err("portable paths reject Windows drive-relative spellings");
    assert!(error.to_string().contains("not portable"), "{error}");

    let mut reserved_windows_source = envelope.clone();
    reserved_windows_source["sources"][0]["path"] = serde_json::json!("src/NUL.loom");
    let error = decode_library_artifact(
        &serde_json::to_vec(&reserved_windows_source).expect("encode reserved Windows source"),
    )
    .expect_err("portable paths reject reserved Windows components");
    assert!(error.to_string().contains("not portable"), "{error}");

    let mut invalid_semver = envelope.clone();
    invalid_semver["rootPackage"]["version"] = serde_json::json!("invalid");
    invalid_semver["packages"][0]["id"]["version"] = serde_json::json!("invalid");
    let mut invalid_requirement = envelope.clone();
    let package_id = invalid_requirement["packages"][0]["id"].clone();
    invalid_requirement["packages"][0]["dependencies"] = serde_json::json!([{
        "alias": "loopback",
        "requirement": "not-semver",
        "package": package_id,
    }]);
    let mut unsatisfied_requirement = envelope.clone();
    let package_id = unsatisfied_requirement["packages"][0]["id"].clone();
    unsatisfied_requirement["packages"][0]["dependencies"] = serde_json::json!([{
        "alias": "loopback",
        "requirement": "^999",
        "package": package_id,
    }]);
    let mut absent_dependency = envelope.clone();
    absent_dependency["packages"][0]["dependencies"] = serde_json::json!([{
        "alias": "absent",
        "requirement": null,
        "package": {"name": "absent", "version": "1.0.0", "language": "0.3"},
    }]);
    let mut unreachable_package = envelope.clone();
    unreachable_package["packages"]
        .as_array_mut()
        .expect("artifact packages")
        .push(serde_json::json!({
            "id": {"name": "orphan", "version": "1.0.0", "language": "0.3"},
            "dependencies": [],
        }));
    let mut dependency_cycle = envelope.clone();
    let package_id = dependency_cycle["packages"][0]["id"].clone();
    dependency_cycle["packages"][0]["dependencies"] = serde_json::json!([{
        "alias": "loopback",
        "requirement": "*",
        "package": package_id,
    }]);
    let mut malformed_source = envelope.clone();
    malformed_source["sources"][0]["text"] = serde_json::json!("module {");
    for (name, forged, expected) in [
        ("invalid SemVer", invalid_semver, "invalid semantic version"),
        (
            "invalid requirement",
            invalid_requirement,
            "invalid requirement",
        ),
        (
            "unsatisfied requirement",
            unsatisfied_requirement,
            "but resolves",
        ),
        ("absent dependency", absent_dependency, "depends on absent"),
        (
            "unreachable package",
            unreachable_package,
            "not reachable from root",
        ),
        ("dependency cycle", dependency_cycle, "contains a cycle"),
        ("malformed source", malformed_source, "does not parse"),
    ] {
        let error = decode_library_artifact(
            &serde_json::to_vec(&forged).expect("encode forged artifact graph"),
        )
        .expect_err("forged artifact graph must be rejected");
        assert!(error.to_string().contains(expected), "{name}: {error}");
    }

    let mut forged_interface = envelope.clone();
    forged_interface["publicInterfaces"][0]["fingerprint"] = serde_json::json!("forged");
    let error = decode_library_artifact(
        &serde_json::to_vec(&forged_interface).expect("encode forged interface artifact"),
    )
    .expect_err("public interfaces are derived from embedded source");
    assert!(
        error
            .to_string()
            .contains("public interfaces do not match embedded source"),
        "{error}"
    );
    let decoded = decode_library_artifact(&bytes).expect("decode package artifact");
    assert_eq!(decoded.root_package().name(), "utility");
    assert_eq!(decoded.root_package().version(), "1.2.0");
    assert_eq!(decoded.root_package().language(), "0.3");
    assert!(
        decoded
            .interfaces()
            .iter()
            .any(|interface| { interface.module.ends_with("::utility.math") })
    );
    assert!(
        decoded
            .interfaces()
            .iter()
            .all(|interface| !interface.module.starts_with("std@")),
        "compiler-owned std interfaces must not be serialized into a portable package"
    );
    assert!(decoded.interfaces().iter().any(|interface| {
        interface.module.ends_with("::utility.nested")
            && interface.files == ["deps/utility@1.2.0/nested.loom"]
    }));
    let decoded_interfaces = decoded.interfaces().to_vec();
    fs::write(project.root.join("utility.loomlib"), bytes).expect("write package artifact");

    project.write(
        "application/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"application\"\nversion = \"0.1.0\"\n[dependencies]\nutility = { artifact = \"../utility.loomlib\", version = \"^1\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.main\"\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.math.increment\n\npub fn main() {\n    let answer = increment(41)\n    assert answer == 42\n}\n",
    );
    fs::remove_dir_all(project.root.join("utility")).expect("remove producer checkout");

    let consumer = AnalysisHost::new(project.root.join("application")).expect("open consumer");
    assert_eq!(consumer.project().packages().count(), 2);
    let snapshot = consumer
        .snapshot()
        .expect("compile from artifact dependency");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let dependency_interfaces = snapshot
        .module_interfaces()
        .into_iter()
        .filter(|interface| interface.module.starts_with("utility@"))
        .collect::<Vec<_>>();
    assert_eq!(dependency_interfaces, decoded_interfaces);
    let dependency = snapshot
        .sources()
        .documents()
        .iter()
        .find(|source| {
            source
                .package()
                .is_some_and(|package| package.name() == "utility")
                && source.relative_path().ends_with("src/math.loom")
        })
        .expect("embedded dependency source");
    assert!(dependency.is_embedded_dependency());
    assert_eq!(dependency.origin(), SourceOrigin::PortableLibrary);
    assert!(dependency.is_read_only());
    assert!(!dependency.is_navigable());
    assert_eq!(
        dependency.relative_path(),
        "deps/utility@1.2.0/src/math.loom"
    );
    assert!(
        dependency
            .absolute_path()
            .to_string_lossy()
            .contains("utility.loomlib")
    );
    let std_sources = snapshot
        .sources()
        .documents()
        .iter()
        .filter(|source| {
            source
                .package()
                .is_some_and(|package| package.name() == "std")
        })
        .collect::<Vec<_>>();
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/int.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/json.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/log.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/process.loom")),
        "{std_sources:#?}"
    );
    assert!(
        std_sources
            .iter()
            .any(|source| source.relative_path().ends_with("src/resource.loom")),
        "{std_sources:#?}"
    );
    for source in std_sources {
        assert_compiler_std_source(source);
        assert!(
            !source
                .absolute_path()
                .to_string_lossy()
                .contains("utility.loomlib")
        );
    }
    let program = snapshot.executable().expect("consumer checked MIR");
    let entry = program.exports["application.main"];
    let value = Interpreter::new(program)
        .invoke(entry, Vec::new(), Span::default())
        .expect("run artifact-backed dependency");
    assert_eq!(value, Value::Unit);

    let cache = PersistentCache::new(project.root.join("semantic-cache"));
    let first_process =
        AnalysisHost::new(project.root.join("application")).expect("open first cached consumer");
    let first_sources = first_process
        .load_sources()
        .expect("load first source graph");
    let (first_cached, _) = first_process.snapshot_from_sources_with_parse_cache(
        first_sources,
        &cache,
        "test-compiler-language-0.3",
    );
    assert!(
        !first_cached.has_errors(),
        "{:#?}",
        first_cached.diagnostics()
    );

    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.math.increment\n\npub fn main() {\n    let answer = increment(40)\n    assert answer == 41\n}\n",
    );
    let second_process =
        AnalysisHost::new(project.root.join("application")).expect("open second cached consumer");
    let second_sources = second_process
        .load_sources()
        .expect("load changed consumer graph");
    let (incremental, _) = second_process.snapshot_from_sources_with_parse_cache(
        second_sources,
        &cache,
        "test-compiler-language-0.3",
    );
    assert!(
        !incremental.has_errors(),
        "{:#?}",
        incremental.diagnostics()
    );
    assert_eq!(incremental.semantic_query_stats().modules_checked, 1);
    assert_eq!(
        incremental.semantic_query_stats().modules_reused,
        incremental.hir().modules.len() - 1
    );
    assert!(incremental.semantic_query_stats().bodies_reused >= 3);
    let program = incremental
        .executable()
        .expect("incremental artifact consumer");
    let entry = program.exports["application.main"];
    assert_eq!(
        Interpreter::new(program)
            .invoke(entry, Vec::new(), Span::default())
            .expect("run incrementally rebuilt consumer"),
        Value::Unit
    );

    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.math.private_value\n\npub fn main() {\n    let hidden = private_value()\n    assert hidden == 99\n}\n",
    );
    let private_import = AnalysisHost::new(project.root.join("application"))
        .expect("open private-import consumer")
        .snapshot()
        .expect("analyze private artifact import");
    assert!(
        private_import
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName"),
        "{:#?}",
        private_import.diagnostics()
    );
}

#[test]
fn portable_library_carries_source_instead_of_process_local_proofs() {
    let project = constrained_proof_project();
    let host = AnalysisHost::new(&project.root).expect("open proof producer");
    let snapshot = host.snapshot().expect("compile proof producer");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let fresh = snapshot.executable().expect("fresh proof checked MIR");
    let fresh_debug = format!("{fresh:#?}");
    assert!(
        fresh_debug.contains("construction: Proven"),
        "{fresh_debug}"
    );
    Interpreter::new(fresh)
        .invoke(fresh.exports["make"], Vec::new(), Span::default())
        .expect("fresh proven construction succeeds");

    let library = encode_library_artifact(snapshot.project(), snapshot.sources())
        .expect("encode proof library");
    let library_json: serde_json::Value =
        serde_json::from_slice(&library).expect("proof library JSON");
    assert_eq!(
        library_json
            .as_object()
            .expect("library envelope")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "format",
            "languageVersion",
            "packages",
            "publicInterfaces",
            "rootPackage",
            "sources",
            "version",
        ]),
        "portable libraries have one exact source-package envelope"
    );
    assert!(
        library_json["sources"]
            .as_array()
            .expect("library sources")
            .iter()
            .any(|source| source["text"]
                .as_str()
                .is_some_and(|text| text.contains("Positive(10.0)")))
    );

    fs::write(project.root.join("proofs.loomlib"), library).expect("write proof artifact");
    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nproofs = { artifact = \"../proofs.loomlib\", version = \"^1\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"consumer.main\"\n",
    );
    project.write(
        "consumer/src/main.loom",
        "module consumer\n\nimport proofs.make\n\npub fn main() {\n    let value = make()\n    assert value >= 0.0\n}\n",
    );
    fs::remove_file(project.root.join("loom.toml")).expect("remove producer manifest");
    fs::remove_dir_all(project.root.join("src")).expect("remove producer checkout");

    let consumer = AnalysisHost::new(project.root.join("consumer"))
        .expect("open proof artifact consumer")
        .snapshot()
        .expect("recheck proof source from artifact");
    assert!(!consumer.has_errors(), "{:#?}", consumer.diagnostics());
    let program = consumer.executable().expect("consumer checked MIR");
    assert!(
        format!("{program:#?}").contains("construction: Proven"),
        "the consumer must derive proof dispositions from embedded source"
    );
    Interpreter::new(program)
        .invoke(
            program.exports["consumer.main"],
            Vec::new(),
            Span::default(),
        )
        .expect("source-recompiled proof library executes");
}

#[test]
#[allow(clippy::too_many_lines)]
fn portable_libraries_compose_shared_transitive_package_content() {
    let project = TestProject::new();
    project.write(
        "leaf/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"leaf\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "leaf/src/lib.loom",
        "module leaf\n\npub fn base() Int { 40 }\n",
    );
    project.write(
        "left/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"left\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { path = \"../leaf\", version = \"^1\" }\n",
    );
    project.write(
        "left/src/lib.loom",
        "module left\n\nimport leaf.base\n\npub fn left_value() Int { base() + 1 }\n",
    );
    project.write(
        "right/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"right\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { path = \"../leaf\", version = \"^1\" }\n",
    );
    project.write(
        "right/src/lib.loom",
        "module right\n\nimport leaf.base\n\npub fn right_value() Int { base() + 2 }\n",
    );

    for package in ["leaf", "left", "right"] {
        let host = AnalysisHost::new(project.root.join(package)).expect("open artifact producer");
        let snapshot = host.snapshot().expect("check artifact producer");
        assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
        let artifact = encode_library_artifact(snapshot.project(), snapshot.sources())
            .expect("encode artifact producer");
        fs::write(project.root.join(format!("{package}.loomlib")), artifact)
            .expect("write portable artifact");
    }
    for package in ["leaf", "left", "right"] {
        fs::remove_dir_all(project.root.join(package)).expect("remove producer checkout");
    }

    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { artifact = \"../leaf.loomlib\", version = \"^1\" }\nleft = { artifact = \"../left.loomlib\", version = \"^1\" }\nright = { artifact = \"../right.loomlib\", version = \"^1\" }\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"consumer.main\"\n",
    );
    project.write(
        "consumer/src/main.loom",
        "module consumer\n\nimport leaf.base\nimport left.left_value\nimport right.right_value\n\npub fn main() {\n    let base_result = base()\n    let left_result = left_value()\n    let right_result = right_value()\n    assert base_result == 40\n    assert left_result == 41\n    assert right_result == 42\n}\n",
    );

    let host = AnalysisHost::new(project.root.join("consumer"))
        .expect("compose direct and transitive artifacts");
    assert_eq!(host.project().packages().count(), 4);
    let snapshot = host.snapshot().expect("check composed artifacts");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("composed artifact MIR");
    assert_eq!(
        Interpreter::new(program)
            .invoke(
                program.exports["consumer.main"],
                Vec::new(),
                Span::default(),
            )
            .expect("run composed artifacts"),
        Value::Unit
    );
}

#[test]
fn portable_library_lock_closes_over_transitive_source_content() {
    let project = TestProject::new();
    project.write(
        "leaf/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"leaf\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "leaf/src/lib.loom",
        "module leaf\n\npub fn base() Int { 40 }\n",
    );
    project.write(
        "bundle/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"bundle\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { path = \"../leaf\", version = \"^1\" }\n",
    );
    project.write(
        "bundle/src/lib.loom",
        "module bundle\n\nimport leaf.base\n\npub fn answer() Int { base() + 1 }\n",
    );

    let artifact_path = project.root.join("bundle.loomlib");
    let build_artifact = || {
        let host = AnalysisHost::new(project.root.join("bundle")).expect("open bundle producer");
        let snapshot = host.snapshot().expect("check bundle producer");
        assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
        encode_library_artifact(snapshot.project(), snapshot.sources())
            .expect("encode bundle artifact")
    };
    fs::write(&artifact_path, build_artifact()).expect("write original bundle");
    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\nbundle = { artifact = \"../bundle.loomlib\", version = \"^1\" }\n",
    );
    project.write("consumer/src/main.loom", "module consumer\n");
    let locked =
        ProjectGraph::load(project.root.join("consumer")).expect("resolve original bundle");
    assert!(locked.write_lockfile().expect("write artifact lock"));

    project.write(
        "leaf/src/lib.loom",
        "module leaf\n\npub fn base() Int { 41 }\n",
    );
    fs::write(&artifact_path, build_artifact()).expect("replace bundle transitive source");
    let error = ProjectGraph::load(project.root.join("consumer"))
        .expect_err("locked artifact rejects changed transitive content");
    assert!(error.to_string().contains("checksum differs"), "{error}");
}

#[test]
fn portable_library_is_bounded_before_file_allocation() {
    let project = TestProject::new();
    let artifact = project.root.join("oversized.loomlib");
    fs::File::create(&artifact)
        .expect("create sparse artifact")
        .set_len(u64::try_from(LIBRARY_ARTIFACT_MAX_BYTES).unwrap() + 1)
        .expect("size sparse artifact beyond the limit");
    project.write(
        "consumer/loom.toml",
        "schema = 1\nlanguage = \"0.3\"\n[package]\nname = \"consumer\"\nversion = \"1.0.0\"\n[dependencies]\noversized = { artifact = \"../oversized.loomlib\" }\n",
    );
    project.write("consumer/src/main.loom", "module consumer\n");

    let error = ProjectGraph::load(project.root.join("consumer"))
        .expect_err("oversized artifact is rejected before reading its contents");
    assert!(
        error.to_string().contains("exceeds the") && error.to_string().contains("byte limit"),
        "{error}"
    );
}

#[test]
fn proof_bearing_compiler_cache_rebuilds_from_source() {
    let project = constrained_proof_project();
    let cache = PersistentCache::new(project.root.join("target/proof-cache"));
    let context = portable_cache_context();
    let cold_host = AnalysisHost::new(&project.root).expect("open cold proof build");
    let cold_sources = cold_host.load_sources().expect("load cold proof sources");
    let key = PersistentCache::compilation_key(cold_host.project(), &cold_sources, &context);
    let (cold, _) = cold_host.snapshot_from_sources_with_parse_cache(
        cold_sources,
        &cache,
        &context.frontend_identity,
    );
    let cold_program = cold.executable().expect("cold proof checked MIR");
    assert!(
        format!("{cold_program:#?}").contains("construction: Proven"),
        "cold source analysis must derive the proof"
    );
    cache
        .store_compilation(&key, cold_program, &cold.diagnostic_records())
        .expect("proof-bearing checked MIR is a non-fatal cache skip");
    assert!(
        matches!(cache.load_compilation(&key), CacheLookup::Miss),
        "a proof-bearing cache entry must never become a warm Recheck hit"
    );

    let warm_host = AnalysisHost::new(&project.root).expect("open warm proof build");
    let warm_sources = warm_host.load_sources().expect("load warm proof sources");
    let (warm, warm_parse_stats) = warm_host.snapshot_from_sources_with_parse_cache(
        warm_sources,
        &cache,
        &context.frontend_identity,
    );
    assert!(warm_parse_stats.is_full_hit());
    assert_eq!(warm.semantic_query_stats().modules_reused, 0);
    assert!(warm.semantic_query_stats().bodies_checked > 0);
    let warm_program = warm.executable().expect("rebuilt warm proof MIR");
    let warm_debug = format!("{warm_program:#?}");
    assert!(warm_debug.contains("construction: Proven"), "{warm_debug}");
    assert!(
        !warm_debug.contains("construction: Recheck"),
        "{warm_debug}"
    );
    assert_eq!(cold.diagnostic_records(), warm.diagnostic_records());
    assert_eq!(format!("{cold_program:#?}"), warm_debug);
    Interpreter::new(warm_program)
        .invoke(warm_program.exports["make"], Vec::new(), Span::default())
        .expect("warm source proof preserves cold execution behavior");
}

#[test]
fn package_imports_are_limited_to_direct_dependencies() {
    let project = TestProject::new();
    project.write(
        "leaf/loom.toml",
        "schema = 1\n[package]\nname = \"leaf\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "leaf/src/lib.loom",
        "module leaf.api\n\npub fn value() Int { 1 }\n",
    );
    project.write(
        "middle/loom.toml",
        "schema = 1\n[package]\nname = \"middle\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
    );
    project.write("middle/src/lib.loom", "module middle\n");
    project.write(
        "root/loom.toml",
        "schema = 1\n[package]\nname = \"root\"\nversion = \"1.0.0\"\n[dependencies]\nmiddle = { path = \"../middle\" }\n",
    );
    project.write(
        "root/src/main.loom",
        "module root\n\nimport leaf.api.value\n\npub fn main() {\n    let answer = value()\n    assert answer == 1\n}\n",
    );

    let snapshot = AnalysisHost::new(project.root.join("root"))
        .expect("open transitive graph")
        .snapshot()
        .expect("analyze transitive import");
    assert!(
        snapshot.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "UndeclaredDependency" && diagnostic.message.contains("leaf@1.0.0")
        }),
        "{:#?}",
        snapshot.diagnostics()
    );
}

#[test]
fn dependency_aliases_resolve_without_exposing_the_package_name() {
    let project = TestProject::new();
    project.write(
        "utility/loom.toml",
        "schema = 1\n[package]\nname = \"utility\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "utility/src/math.loom",
        "module utility.math\n\npub fn increment(value Int) Int { value + 1 }\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\nutil = { path = \"../utility\", package = \"utility\" }\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nimport util.math.increment\n\npub fn main() {\n    let answer = increment(1)\n    assert answer == 2\n}\n",
    );

    let snapshot = AnalysisHost::new(project.root.join("application"))
        .expect("open aliased graph")
        .snapshot()
        .expect("analyze aliased import");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
}

#[test]
fn package_sources_cannot_claim_another_package_namespace() {
    let project = TestProject::new();
    project.write(
        "dependency/loom.toml",
        "schema = 1\n[package]\nname = \"dependency\"\nversion = \"1.0.0\"\n",
    );
    project.write(
        "dependency/src/lib.loom",
        "module application\n\npub fn hijack() {}\n",
    );
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"1.0.0\"\n[dependencies]\ndependency = { path = \"../dependency\" }\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\npub fn main() {}\n",
    );

    let snapshot = AnalysisHost::new(project.root.join("application"))
        .expect("open namespaced graph")
        .snapshot()
        .expect("analyze package namespace");
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| { diagnostic.code == "PackageModuleNamespace" }),
        "{:#?}",
        snapshot.diagnostics()
    );
}

#[test]
fn library_targets_reject_executable_entries() {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[[target]]\nname = \"api\"\nkind = \"lib\"\nentry = \"sample.main\"\n",
    );
    project.write("src/lib.loom", "module sample\n");
    let error = AnalysisHost::new(&project.root)
        .err()
        .expect("library entry is rejected");
    assert!(
        error
            .to_string()
            .contains("library target `api` cannot declare entry"),
        "{error}"
    );
}

#[test]
fn manifest_fails_closed_on_version_mismatch_and_dependency_cycle() {
    let mismatch = TestProject::new();
    mismatch.write(
        "dependency/loom.toml",
        "schema = 1\n[package]\nname = \"dependency\"\nversion = \"2.0.0\"\n",
    );
    mismatch.write("dependency/src/lib.loom", "module dependency\n");
    mismatch.write(
        "root/loom.toml",
        "schema = 1\n[package]\nname = \"root\"\nversion = \"0.1.0\"\n[dependencies]\ndependency = { path = \"../dependency\", version = \"^1\" }\n",
    );
    mismatch.write("root/src/main.loom", "module root\n");
    let error = AnalysisHost::new(mismatch.root.join("root"))
        .err()
        .expect("version mismatch is rejected");
    assert!(error.to_string().contains("requires `^1`"), "{error}");

    let cycle = TestProject::new();
    cycle.write(
        "a/loom.toml",
        "schema = 1\n[package]\nname = \"a\"\nversion = \"0.1.0\"\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    cycle.write("a/src/a.loom", "module a\n");
    cycle.write(
        "b/loom.toml",
        "schema = 1\n[package]\nname = \"b\"\nversion = \"0.1.0\"\n[dependencies]\na = { path = \"../a\" }\n",
    );
    cycle.write("b/src/b.loom", "module b\n");
    let error = AnalysisHost::new(cycle.root.join("a"))
        .err()
        .expect("dependency cycle is rejected");
    assert!(error.to_string().contains("dependency cycle"), "{error}");
}

#[test]
fn language_version_defaults_to_current_and_rejects_unknown_versions() {
    let defaulted = TestProject::new();
    defaulted.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"1.0.0\"\n",
    );
    defaulted.write("src/lib.loom", "module sample\n");
    let graph = ProjectGraph::load(&defaulted.root).expect("default language version");
    assert_eq!(graph.language_version(), "0.3");
    assert_eq!(
        graph.root_package().expect("root package").id().language(),
        "0.3"
    );
    assert!(graph.write_lockfile().expect("write versioned lockfile"));
    let lock = fs::read_to_string(defaulted.root.join("loom.lock")).expect("read lockfile");
    assert!(lock.contains("language = \"0.3\""), "{lock}");

    let future = TestProject::new();
    future.write(
        "loom.toml",
        "schema = 1\nlanguage = \"0.4\"\n[package]\nname = \"future\"\nversion = \"1.0.0\"\n",
    );
    future.write("src/lib.loom", "module future\n");
    let error = ProjectGraph::load(&future.root).expect_err("future language must fail closed");
    assert_eq!(error.code(), "UnsupportedLanguageVersion");
    assert!(
        error.to_string().contains("`0.4`") && error.to_string().contains("`0.3`"),
        "{error}"
    );
}

#[test]
fn lockfile_requires_package_entries() {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"1.0.0\"\n",
    );
    project.write("src/lib.loom", "module sample\n");
    project.write("loom.lock", "schema = 1\n");

    let error = ProjectGraph::load(&project.root).expect_err("package entries are required");
    assert_eq!(error.code(), "ProjectLoadFailed");
    assert!(
        error.to_string().contains("invalid lockfile")
            && error.to_string().contains("missing field `package`"),
        "{error}"
    );
}

#[test]
fn locked_packages_require_language() {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"1.0.0\"\n",
    );
    project.write("src/lib.loom", "module sample\n");
    project.write(
        "loom.lock",
        "schema = 1\n\n[[package]]\nname = \"sample\"\nversion = \"1.0.0\"\nsource = \"root\"\n",
    );

    let error = ProjectGraph::load(&project.root).expect_err("package language is required");
    assert_eq!(error.code(), "ProjectLoadFailed");
    assert!(
        error.to_string().contains("invalid lockfile")
            && error.to_string().contains("missing field `language`"),
        "{error}"
    );
}

#[test]
fn language_version_changes_compilation_cache_identity() {
    let project = TestProject::new();
    project.write("main.loom", "module sample\n");
    let host = AnalysisHost::new(&project.root).expect("open cache project");
    let sources = host.load_sources().expect("load cache sources");
    let current = portable_cache_context();
    let mut future = current.clone();
    future.language_version = "0.4".to_owned();
    assert_ne!(
        PersistentCache::compilation_key(host.project(), &sources, &current),
        PersistentCache::compilation_key(host.project(), &sources, &future)
    );
}

#[test]
fn frontend_stdlib_and_contract_change_compilation_identity() {
    let project = TestProject::new();
    project.write("main.loom", "module sample\n");
    let host = AnalysisHost::new(&project.root).expect("open cache project");
    let sources = host.load_sources().expect("load cache sources");
    let current = portable_cache_context();
    let current_key = PersistentCache::compilation_key(host.project(), &sources, &current);

    let mut changed_frontend = current.clone();
    changed_frontend.frontend_identity = "test-frontend-v2".to_owned();
    assert_ne!(
        current_key,
        PersistentCache::compilation_key(host.project(), &sources, &changed_frontend)
    );

    let mut changed_stdlib = current.clone();
    changed_stdlib.stdlib_identity = "test-stdlib-v2".to_owned();
    assert_ne!(
        current_key,
        PersistentCache::compilation_key(host.project(), &sources, &changed_stdlib)
    );

    let mut changed_contract_mode = current.clone();
    changed_contract_mode.contract_mode = "unchecked".to_owned();
    assert_ne!(
        current_key,
        PersistentCache::compilation_key(host.project(), &sources, &changed_contract_mode)
    );
}

#[test]
fn manifest_target_declarations_do_not_change_compilation_identity() {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[[target]]\nname = \"first\"\nkind = \"bin\"\nentry = \"sample.start\"\n",
    );
    project.write(
        "src/main.loom",
        "module sample\n\npub fn start() {\n}\n\npub fn alternate() {\n}\n",
    );
    let first_host = AnalysisHost::new(&project.root).expect("open first target graph");
    let first_sources = first_host
        .load_sources()
        .expect("load first target sources");
    let context = portable_cache_context();
    let first_key =
        PersistentCache::compilation_key(first_host.project(), &first_sources, &context);
    assert_eq!(first_host.project().targets()[0].name(), "first");
    assert_eq!(
        first_host.project().targets()[0].entry(),
        Some("sample.start")
    );

    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[[target]]\nname = \"renamed\"\nkind = \"bin\"\nentry = \"sample.alternate\"\n",
    );
    let second_host = AnalysisHost::new(&project.root).expect("open changed target graph");
    let second_sources = second_host
        .load_sources()
        .expect("load changed target sources");
    let second_key =
        PersistentCache::compilation_key(second_host.project(), &second_sources, &context);
    assert_eq!(second_host.project().targets()[0].name(), "renamed");
    assert_eq!(
        second_host.project().targets()[0].entry(),
        Some("sample.alternate")
    );
    assert_eq!(
        first_key, second_key,
        "target selection belongs to the artifact layer, not checked MIR"
    );
}

#[test]
fn registry_features_and_lockfile_form_a_reproducible_graph() {
    let project = TestProject::new();
    let write_registry_version = |version: &str, value: i64| {
        project.write(
            &format!("registry/utility/{version}/loom.toml"),
            &format!("schema = 1\n[package]\nname = \"utility\"\nversion = \"{version}\"\n"),
        );
        project.write(
            &format!("registry/utility/{version}/src/lib.loom"),
            &format!("module utility\n\npub fn value() Int {{\n    {value}\n}}\n"),
        );
    };
    write_registry_version("1.0.0", 10);
    write_registry_version("1.2.0", 12);
    project.write(
        "application/loom.toml",
        "schema = 1\n[package]\nname = \"application\"\nversion = \"0.1.0\"\n[registries]\nlocal = \"../registry\"\n[dependencies]\nutility = { registry = \"local\", version = \"^1\", optional = true }\n[features]\ndefault = [\"utilities\"]\nutilities = [\"dep:utility\"]\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.main\"\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\npub fn main() {\n}\n",
    );
    let root = project.root.join("application");

    let without_defaults = ProjectGraph::load_with_options(
        &root,
        &ProjectOptions {
            no_default_features: true,
            ..ProjectOptions::default()
        },
    )
    .expect("resolve with defaults disabled");
    assert_eq!(without_defaults.packages().count(), 1);

    let graph = ProjectGraph::load(&root).expect("resolve newest registry package");
    let utility = graph
        .packages()
        .find(|package| package.id().name() == "utility")
        .expect("default feature activates utility");
    assert_eq!(utility.id().version(), "1.2.0");
    assert_eq!(utility.source(), "registry+local");
    assert_eq!(utility.checksum().map(str::len), Some(64));
    assert!(graph.write_lockfile().expect("write lockfile"));
    assert!(!graph.write_lockfile().expect("lockfile is idempotent"));
    let lock = fs::read_to_string(root.join("loom.lock")).expect("read lockfile");
    assert!(lock.contains("version = \"1.2.0\""), "{lock}");
    assert!(lock.contains("source = \"registry+local\""), "{lock}");

    write_registry_version("1.3.0", 13);
    let pinned = ProjectGraph::load(&root).expect("existing lock pins registry version");
    assert!(
        pinned.packages().any(|package| {
            package.id().name() == "utility" && package.id().version() == "1.2.0"
        })
    );

    let refreshed = ProjectGraph::load_with_options(
        &root,
        &ProjectOptions {
            lock_mode: LockMode::Refresh,
            ..ProjectOptions::default()
        },
    )
    .expect("refresh registry resolution");
    assert!(
        refreshed.packages().any(|package| {
            package.id().name() == "utility" && package.id().version() == "1.3.0"
        })
    );
    assert!(refreshed.write_lockfile().expect("publish refreshed lock"));
    ProjectGraph::load_with_options(
        &root,
        &ProjectOptions {
            lock_mode: LockMode::Locked,
            ..ProjectOptions::default()
        },
    )
    .expect("fresh lock passes locked mode");

    project.write(
        "registry/utility/1.3.0/src/lib.loom",
        "module utility\n\npub fn value() Int {\n    99\n}\n",
    );
    let error = ProjectGraph::load(&root)
        .expect_err("published registry contents cannot mutate under a lock");
    assert!(error.to_string().contains("checksum differs"), "{error}");
}

#[test]
fn feature_graph_rejects_unknown_dependencies_and_cycles() {
    let project = TestProject::new();
    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[features]\ndefault = [\"a\"]\na = [\"b\"]\nb = [\"a\"]\n",
    );
    project.write("src/lib.loom", "module sample\n");
    let error = ProjectGraph::load(&project.root).expect_err("feature cycle is rejected");
    assert!(error.to_string().contains("feature cycle"), "{error}");

    project.write(
        "loom.toml",
        "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n[features]\ndefault = [\"missing\"]\n",
    );
    let error = ProjectGraph::load(&project.root).expect_err("unknown feature is rejected");
    assert!(error.to_string().contains("unknown feature"), "{error}");
}

#[test]
fn persistent_cache_is_relocatable_and_corruption_is_a_safe_miss() {
    let first = TestProject::new();
    let second = TestProject::new();
    for project in [&first, &second] {
        project.write(
            "loom.toml",
            "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
        );
        project.write("src/main.loom", "module sample\n\npub fn main() {\n}\n");
    }

    let first_host = AnalysisHost::new(&first.root).expect("open first relocation");
    let first_sources = first_host.load_sources().expect("load first sources");
    let second_host = AnalysisHost::new(&second.root).expect("open second relocation");
    let second_sources = second_host.load_sources().expect("load second sources");
    let context = portable_cache_context();
    let first_key =
        PersistentCache::compilation_key(first_host.project(), &first_sources, &context);
    let second_key =
        PersistentCache::compilation_key(second_host.project(), &second_sources, &context);
    assert_eq!(first_key, second_key, "absolute paths must not affect keys");

    let snapshot = first_host.snapshot_from_sources(first_sources);
    assert!(!snapshot.has_errors());
    let cache = PersistentCache::new(first.root.join("target/explicit-cache"));
    cache
        .store_compilation(
            &first_key,
            snapshot.executable().expect("checked MIR"),
            &snapshot.diagnostic_records(),
        )
        .expect("store checked MIR");
    let CacheLookup::Hit(cached) = cache.load_compilation(&first_key) else {
        panic!("stored checked MIR must hit");
    };
    assert!(cached.program().exports.contains_key("sample.main"));

    let blob_root = cache.root().join("blobs/sha256");
    let shard = fs::read_dir(&blob_root)
        .expect("read blob root")
        .next()
        .expect("one blob shard")
        .expect("read shard")
        .path();
    let blob = fs::read_dir(shard)
        .expect("read blob shard")
        .next()
        .expect("one blob")
        .expect("read blob")
        .path();
    fs::write(&blob, b"corrupt").expect("corrupt cache blob");
    assert!(matches!(
        cache.load_compilation(&first_key),
        CacheLookup::Miss
    ));

    cache
        .store_compilation(
            &first_key,
            snapshot.executable().expect("checked MIR"),
            &snapshot.diagnostic_records(),
        )
        .expect("a later store repairs a corrupt CAS blob");
    assert!(cache.load_compilation(&first_key).is_hit());

    first.write(
        "src/main.loom",
        "module sample\n\npub fn main() {\n    assert true\n}\n",
    );
    let changed_host = AnalysisHost::new(&first.root).expect("reopen changed project");
    let changed_sources = changed_host.load_sources().expect("load changed source");
    let changed_key =
        PersistentCache::compilation_key(changed_host.project(), &changed_sources, &context);
    assert_ne!(changed_key, first_key, "source content must affect keys");
}

#[test]
fn per_source_parse_cache_skips_lexing_and_parsing_on_a_graph_miss() {
    let project = TestProject::new();
    project.write("main.loom", "module sample\n\npub fn main() {\n}\n");
    let host = AnalysisHost::new(&project.root).expect("open cached parse project");
    let cache = PersistentCache::new(project.root.join("target/parse-cache"));
    let context = portable_cache_context();

    let first_sources = host.load_sources().expect("load first source set");
    let source_count = first_sources.documents().len();
    let (first, first_stats) = host.snapshot_from_sources_with_parse_cache(
        first_sources,
        &cache,
        &context.frontend_identity,
    );
    assert!(!first.has_errors(), "{:?}", first.diagnostics());
    assert_eq!(first_stats.hits, 0);
    assert_eq!(first_stats.misses, source_count);

    let second_sources = host.load_sources().expect("load second source set");
    assert_eq!(second_sources.documents().len(), source_count);
    let (second, second_stats) = host.snapshot_from_sources_with_parse_cache(
        second_sources,
        &cache,
        &context.frontend_identity,
    );
    assert!(!second.has_errors(), "{:?}", second.diagnostics());
    assert!(second_stats.is_full_hit());
    assert_eq!(second_stats.hits, source_count);
    let root_file = first
        .sources()
        .documents()
        .iter()
        .find(|source| source.is_root_package())
        .expect("root source")
        .id();
    assert_eq!(
        first.parse(root_file).expect("first parse").ast(),
        second.parse(root_file).expect("cached parse").ast()
    );
}

#[test]
fn persistent_semantic_reuse_rederives_compiler_owned_must_scope_identity() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module application\n\npub fn main() {\n    let value = 1\n}\n",
    );
    let cache = PersistentCache::new(project.root.join("target/semantic-resource-cache"));

    let cold_host = AnalysisHost::new(&project.root).expect("open cold resource project");
    let (cold, _) = cold_host.snapshot_from_sources_with_parse_cache(
        cold_host.load_sources().expect("load cold resource source"),
        &cache,
        "must-scope-identity-test-v1",
    );
    assert!(!cold.has_errors(), "{:#?}", cold.diagnostics());

    project.write(
        "main.loom",
        "module application\n\npub fn main() {\n    let value = 2\n}\n",
    );
    let warm_host = AnalysisHost::new(&project.root).expect("open warm resource project");
    let (warm, _) = warm_host.snapshot_from_sources_with_parse_cache(
        warm_host.load_sources().expect("load warm resource source"),
        &cache,
        "must-scope-identity-test-v1",
    );
    assert!(!warm.has_errors(), "{:#?}", warm.diagnostics());
    assert_eq!(warm.semantic_query_stats().modules_checked, 1);
    assert_eq!(
        warm.semantic_query_stats().modules_reused,
        warm.hir().modules.len() - 1
    );
    let semantic_id = warm
        .semantic_analysis()
        .canonical_concepts
        .must_scope
        .expect("semantic analysis must rederive MustScope from current HIR");
    assert_eq!(
        warm.hir().modules[warm.hir().definitions[semantic_id].module]
            .name
            .as_str(),
        "std.resource"
    );
    let mir = warm.executable().expect("warm checked MIR");
    let marker = mir
        .concept(mir.prelude.must_scope_concept.expect("MIR MustScope id"))
        .expect("MIR MustScope concept");
    assert_eq!(marker.module, "std.resource");
    assert_eq!(marker.identity, Some(ConceptIdentity::MustScope));
}

#[test]
fn warm_semantic_reanalysis_preserves_task_std_item_identity() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module cached_task_item\n\nasync fn child() Int { 1 }\n\npub async fn main() {\n    discard Task.any(child(), child()).await\n}\n",
    );
    let cache = PersistentCache::new(project.root.join("target/task-item-cache"));
    let compile = || {
        let host = AnalysisHost::new(&project.root).expect("open task item project");
        host.snapshot_from_sources_with_parse_cache(
            host.load_sources().expect("load task item source"),
            &cache,
            "task-std-item-test-v1",
        )
        .0
    };

    let cold = compile();
    assert!(!cold.has_errors(), "{:#?}", cold.diagnostics());
    let warm = compile();
    assert!(!warm.has_errors(), "{:#?}", warm.diagnostics());
    assert_eq!(warm.semantic_query_stats().modules_checked, 0);
    assert_eq!(
        warm.semantic_query_stats().modules_reused,
        warm.hir().modules.len()
    );

    let items = |snapshot: &loom_driver::AnalysisSnapshot| {
        snapshot
            .semantic_analysis()
            .typed
            .bodies
            .values()
            .flat_map(|body| body.calls.values())
            .filter_map(|call| match call.target {
                CallTarget::TaskIntrinsic(item) => Some(item),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(items(&cold), [TaskIntrinsic::Any]);
    assert_eq!(items(&warm), [TaskIntrinsic::Any]);
    warm.executable()
        .expect("cached task item identity must lower to checked MIR");
}

#[test]
fn module_interface_ignores_bodies_but_tracks_public_contracts() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module sample\n\npub fn value(input Int) Int {\n    input + 1\n}\n\nfn private() Int {\n    1\n}\n",
    );
    let first = AnalysisHost::new(&project.root)
        .expect("open first interface")
        .snapshot()
        .expect("compile first interface")
        .module_interfaces();

    project.write(
        "main.loom",
        "module sample\n\npub fn value(input Int) Int {\n    input + 2\n}\n\nfn private() Int {\n    2\n}\n",
    );
    let bodies_changed = AnalysisHost::new(&project.root)
        .expect("open body-edited interface")
        .snapshot()
        .expect("compile body-edited interface")
        .module_interfaces();
    assert_eq!(first, bodies_changed);

    project.write(
        "main.loom",
        "module sample\n\npub fn value(input Int) Int\n    requires input >= 0\n{\n    input + 2\n}\n",
    );
    let contract_changed = AnalysisHost::new(&project.root)
        .expect("open contract-edited interface")
        .snapshot()
        .expect("compile contract-edited interface")
        .module_interfaces();
    assert_ne!(first[0].fingerprint, contract_changed[0].fingerprint);
}

#[test]
fn typed_hir_queries_reuse_unmodified_modules() {
    let project = TestProject::new();
    project.write(
        "a.loom",
        "module sample.a\n\npub fn value(input Int) Int {\n    input + 1\n}\n",
    );
    project.write(
        "b.loom",
        "module sample.b\n\nimport sample.a.value\n\npub fn main() {\n    let output = value(1)\n    assert output > 0\n}\n\ntest fn calls_dependency() {\n    main()\n}\n",
    );
    let host = AnalysisHost::new(&project.root).expect("open incremental project");
    let first = host.snapshot().expect("compile initial graph");
    assert!(!first.has_errors(), "{:#?}", first.diagnostics());
    let module_count = first.hir().modules.len();
    let first_stats = first.semantic_query_stats();
    assert_eq!(first_stats.modules_checked, module_count);
    assert_eq!(first_stats.modules_reused, 0);

    let unchanged = host.snapshot().expect("compile unchanged graph");
    let unchanged_stats = unchanged.semantic_query_stats();
    assert_eq!(unchanged_stats.modules_reused, module_count);
    assert_eq!(unchanged_stats.bodies_checked, 0);

    project.write(
        "a.loom",
        "module sample.a\n\npub fn value(input Int) Int {\n    input + 2\n}\n",
    );
    let changed = host.snapshot().expect("compile one changed module");
    assert!(!changed.has_errors(), "{:#?}", changed.diagnostics());
    let changed_stats = changed.semantic_query_stats();
    assert_eq!(changed_stats.modules_checked, 1);
    assert_eq!(changed_stats.modules_reused, module_count - 1);
    assert!(changed_stats.bodies_reused >= 2);
    let program = changed.executable().expect("incremental checked MIR");
    let function = program.exports["sample.a.value"];
    let result = Interpreter::new(program)
        .invoke(function, vec![Value::Int { value: 1 }], Span::default())
        .expect("invoke changed body");
    assert_eq!(result, Value::Int { value: 3 });
}

#[test]
fn concurrent_cache_writers_publish_only_complete_blobs_and_refs() {
    let project = TestProject::new();
    project.write("main.loom", "module sample\n\npub fn main() {\n}\n");
    let host = AnalysisHost::new(&project.root).expect("open concurrent cache project");
    let sources = host.load_sources().expect("load concurrent cache sources");
    let context = portable_cache_context();
    let key = PersistentCache::compilation_key(host.project(), &sources, &context);
    let snapshot = host.snapshot_from_sources(sources);
    let program = Arc::new(snapshot.executable().expect("checked MIR").clone());
    let diagnostics = Arc::new(snapshot.diagnostic_records());
    let cache = Arc::new(PersistentCache::new(project.root.join("target/racy-cache")));
    let object_key = PersistentCache::derived_key(&key, &[("layer", "object-stress")]);
    let artifact_key = PersistentCache::derived_key(&key, &[("layer", "artifact-stress")]);
    let barrier = Arc::new(Barrier::new(12));

    std::thread::scope(|scope| {
        for _ in 0..12 {
            let cache = Arc::clone(&cache);
            let program = Arc::clone(&program);
            let diagnostics = Arc::clone(&diagnostics);
            let key = key.clone();
            let object_key = object_key.clone();
            let artifact_key = artifact_key.clone();
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..16 {
                    cache
                        .store_compilation(&key, &program, &diagnostics)
                        .expect("concurrent checked-MIR store");
                    cache
                        .store_target_object(&object_key, b"stable-object-bytes")
                        .expect("concurrent object store");
                    cache
                        .store_artifact(&artifact_key, b"stable-artifact-bytes")
                        .expect("concurrent artifact store");
                }
            });
        }
    });

    assert!(cache.load_compilation(&key).is_hit());
    assert!(matches!(
        cache.load_target_object(&object_key),
        CacheLookup::Hit(bytes) if bytes == b"stable-object-bytes"
    ));
    assert!(matches!(
        cache.load_artifact(&artifact_key),
        CacheLookup::Hit(bytes) if bytes == b"stable-artifact-bytes"
    ));
}

#[test]
fn many_modules_and_call_edges_close_the_checked_mir_pipeline() {
    const MODULES: usize = 48;
    let project = TestProject::new();
    for index in 0..MODULES {
        let source = if index + 1 == MODULES {
            format!("module stress.m{index}\n\npub fn f{index}(value Int) Int {{\n    value\n}}\n")
        } else {
            let next = index + 1;
            format!(
                "module stress.m{index}\n\nimport stress.m{next}.f{next}\n\npub fn f{index}(value Int) Int {{\n    f{next}(value) + 1\n}}\n"
            )
        };
        project.write(&format!("src/m{index}.loom"), &source);
    }
    project.write(
        "src/main.loom",
        &format!(
            "module stress.app\n\nimport stress.m0.f0\n\npub fn main() {{\n    let answer = f0(0)\n    assert answer == {MODULES_MINUS_ONE}\n}}\n",
            MODULES_MINUS_ONE = MODULES - 1
        ),
    );

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open scale project")
        .snapshot()
        .expect("compile scale project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("scale project checked MIR");
    assert_eq!(
        program
            .functions
            .iter()
            .filter(|function| !function.name.starts_with("std."))
            .count(),
        MODULES + 1
    );
    assert!(program.exports.contains_key("stress.app.main"));
}

#[test]
fn snapshot_assigns_file_ids_by_stable_relative_path_and_builds_executable_mir() {
    let project = TestProject::new();
    project.write("b.loom", "module b\n\nfn b() {\n}\n");
    project.write("a.loom", "module a\n\nfn a() {\n}\n");

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open host")
        .snapshot()
        .expect("build snapshot");
    let paths = snapshot
        .sources()
        .documents()
        .iter()
        .map(|source| (source.id(), source.relative_path()))
        .collect::<Vec<_>>();
    assert!(paths.windows(2).all(|pair| pair[0].1 < pair[1].1));
    assert!(paths.iter().enumerate().all(|(index, (file, _))| {
        *file == FileId(u32::try_from(index).expect("source count fits FileId"))
    }));
    assert_eq!(paths[0], (FileId(0), "a.loom"));
    assert_eq!(paths[1], (FileId(1), "b.loom"));
    for expected in [
        "deps/std@0.3/src/int.loom",
        "deps/std@0.3/src/json.loom",
        "deps/std@0.3/src/log.loom",
        "deps/std@0.3/src/process.loom",
        "deps/std@0.3/src/resource.loom",
    ] {
        assert!(paths.iter().any(|(_, path)| *path == expected), "{paths:?}");
    }
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    assert_eq!(snapshot.completed_stage(), PipelineStage::Executable);
    snapshot
        .require_stage(PipelineStage::TypeChecked)
        .expect("the public semantic analysis is connected");
    snapshot
        .require_stage(PipelineStage::Executable)
        .expect("validated MIR lowering is connected");
    snapshot
        .executable()
        .expect("snapshot exposes executable MIR")
        .validate()
        .expect("driver never publishes unchecked MIR");
}

#[test]
fn overlays_and_cli_snapshots_use_the_same_source_map() {
    let project = TestProject::new();
    project.write("main.loom", "module demo\n\nfn main() {\n}\n");
    let path = project.root.join("main.loom");
    let mut host = AnalysisHost::new(&project.root).expect("open host");
    host.set_overlay(&path, "module demo\n\nfn broken(")
        .expect("set overlay");
    let dirty = host.snapshot().expect("dirty snapshot");
    assert!(dirty.has_errors());
    assert!(
        dirty.executable().is_err(),
        "an error-recovered source tree must never become executable"
    );
    let dirty_file = dirty
        .sources()
        .file_id(&path)
        .expect("overlay source remains in the source map");

    host.clear_overlay(&path).expect("clear overlay");
    let clean = host.snapshot().expect("clean snapshot");
    assert_eq!(clean.sources().file_id(&path), Some(dirty_file));
    assert!(!clean.has_errors(), "{:?}", clean.diagnostics());
    clean
        .executable()
        .expect("clearing the overlay restores executable MIR");
}

#[test]
fn formatter_is_canonical_idempotent_and_refuses_broken_source() {
    let source = "module demo\r\n\r\nfn value() Int { 1 }\r\n\r\nfn main() {\r\n\tdiscard value()   \r\n}\r\n\r\n";
    let first = format_source(FileId(0), source);
    assert!(first.diagnostics.is_empty());
    assert_eq!(
        first.text,
        "module demo\n\nfn value() Int { 1 }\n\nfn main() {\n    discard value()\n}\n"
    );
    let second = format_source(FileId(0), &first.text);
    assert_eq!(second.text, first.text);

    let broken = "fn incomplete(";
    let result = format_source(FileId(0), broken);
    assert!(!result.diagnostics.is_empty());
    assert_eq!(result.text, broken);
}

#[test]
fn source_map_converts_utf16_positions_without_splitting_scalars() {
    let project = TestProject::new();
    project.write("unicode.loom", "module 示例\n");
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open host")
        .snapshot()
        .expect("snapshot");
    let source = snapshot
        .sources()
        .documents()
        .iter()
        .find(|source| source.is_root_package())
        .expect("root unicode source");
    let byte = source
        .byte_offset_utf16(Position {
            line: 0,
            character: 8,
        })
        .expect("valid UTF-16 boundary");
    assert_eq!(
        &source.text().expect("valid source")[byte as usize..],
        "例\n"
    );
}

#[test]
fn repository_examples_cross_the_parse_and_hir_boundary() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let paths = discover_loom_files(&examples).expect("discover checked-in examples");
    assert!(
        !paths.is_empty(),
        "the executable examples are part of the contract"
    );

    let mut parses = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let source = fs::read_to_string(path).expect("example is UTF-8");
        let file = FileId(u32::try_from(index).expect("example FileId fits u32"));
        let parse = parse_with_file(file, &source);
        assert!(
            parse.diagnostics().is_empty(),
            "{}: {:?}",
            path.display(),
            parse.diagnostics()
        );
        parses.push((file, parse));
    }

    let lowered = lower_files(parses.iter().map(|(file, parse)| SourceUnit {
        file: *file,
        syntax: parse.ast(),
    }));
    assert!(
        lowered.diagnostics.is_empty(),
        "example lowering diagnostics: {:?}",
        lowered.diagnostics
    );
}

#[test]
fn async_tasks_and_lexical_defer_execute_from_source() {
    let project = TestProject::new();
    project.write(
        "async.loom",
        r#"module driver.async_cleanup

import std.resource.Dispose
import std.resource.MustScope

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) {
        self.value = 0
    }
}

impl MustScope for Resource {}

async fn child() Int {
    7
}

async fn label() Text {
    "loom"
}

async fn parent() Int {
    child().await + 1
}

fn cleanupOrder() Int {
    var order = 0
    {
        defer {
            order = order * 10 + 1
        }
        defer {
            order = order * 10 + 2
        }
        Unit
    }
    let observed = order
    observed
}

test async fn task_and_cleanup() {
    let observed = cleanupOrder()
    assert observed == 21
    scoped resource = Resource { value = 3 }
    let value = parent().await
    assert value == 8
    let number, text = Task.all(child(), label()).await
    assert number == 7
    assert text == "loom"
}
"#,
    );

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open async project")
        .snapshot()
        .expect("compile async project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute async test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn stored_tasks_and_all_join_modes_execute_from_source() {
    let project = TestProject::new();
    project.write(
        "joins.loom",
        r"module joins

async fn one() Int {
    Task.sleep(2).await
    1
}

async fn two() Int {
    // An immediately ready sibling makes Task.any deterministic without using
    // wall-clock timing as an ordering oracle.
    2
}

test async fn stored_and_dynamic_joins() {
    Task.sleep(1).await
    let first = one()
    let second = two()
    let values = Task.all([first, second]).await

    let combined = Task.all(one(), two())
    let left, right = combined.await
    assert left == 1
    assert right == 2

    let winner = Task.any([one(), two()]).await
    assert winner == 2

    let settled = Task.settled([one(), two()])
    let outcomes = settled.await

    let raced = Task.race([one(), two()])
    let outcome = raced.await
}
",
    );

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open join project")
        .snapshot()
        .expect("compile join project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute join test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn task_outcomes_are_closed_source_values_with_fault_details() {
    let project = TestProject::new();
    project.write(
        "outcomes.loom",
        r#"module outcomes

async fn completed() Int {
    7
}

async fn faulted() Int {
    assert false
    0
}

test async fn inspect_outcomes() {
    let success, failure = Task.settled(completed(), faulted()).await
    match success {
        Completed(value) => {
            assert value == 7
            Unit
        }
        Faulted(_) => {
            assert false
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
    match failure {
        Completed(_) => {
            assert false
            Unit
        }
        Faulted(fault) => {
            let code = fault.code()
            let message = fault.message()
            assert code == "AssertionFault"
            assert message == "assertion was not satisfied"
            Unit
        }
        Cancelled => {
            assert false
            Unit
        }
    }
}
"#,
    );

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open outcome project")
        .snapshot()
        .expect("compile outcome project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute outcome test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn duration_file_and_socket_tasks_execute_from_source() {
    use std::io::{Read, Write};

    let project = TestProject::new();
    let file = project.root.join("round-trip.txt");
    let file_literal = loom_text_literal(
        file.to_str()
            .expect("temporary I/O path must be valid UTF-8"),
    );
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept test client");
        let mut request = [0_u8; 4];
        socket.read_exact(&mut request).expect("read request");
        assert_eq!(&request, b"ping");
        socket.write_all(b"pong").expect("write response");
    });
    let source = format!(
        r#"module std_io

import std.time.milliseconds
import std.file.open_read
import std.file.create
import std.net.connect

test async fn real_io() {{
    let delay = milliseconds(1)
    let observed = delay.as_milliseconds()
    assert observed == 1
    Task.sleep(delay).await
    {{
        scoped output = create("{file_literal}").await
        output.write_text("hello from loom").await
        Unit
    }}
    {{
        scoped input = open_read("{file_literal}").await
        let content = input.read_text().await
        assert content == "hello from loom"
        Unit
    }}
    {{
        scoped socket = connect("127.0.0.1", {port}).await
        socket.write_text("ping").await
        let response = socket.read_text().await
        assert response == "pong"
        Unit
    }}
}}
"#,
    );
    project.write("io.loom", &source);

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open I/O project")
        .snapshot()
        .expect("compile I/O project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute I/O test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
    server.join().expect("join test server");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hello from loom");
}

#[test]
fn compiler_known_resources_require_scoped_and_reject_manual_close() {
    let project = TestProject::new();
    project.write(
        "resources.loom",
        r#"module resources

import std.file.open_read
import std.net.connect

async fn leakedFile() {
    let file = open_read("missing.txt").await
}

async fn discardedSocket() {
    connect("127.0.0.1", 1).await
}

async fn closedTwice() {
    scoped file = open_read("missing.txt").await
    file.close()
}
"#,
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open resource diagnostics project")
        .snapshot()
        .expect("analyze resource diagnostics project");
    let codes = snapshot
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"MustScopeRequiresScoped"), "{codes:?}");
    assert!(codes.contains(&"ManualDisposeOfScopedValue"), "{codes:?}");
    assert!(snapshot.executable().is_err());
}

#[test]
fn cancellation_runs_registered_cleanup_before_join_finishes() {
    let project = TestProject::new();
    project.write(
        "cancel.loom",
        r"module cancellation

async fn slow() Int {
    var cleaned = false
    defer {
        cleaned = true
    }
    Task.sleep(100).await
    1
}

async fn fast() Int {
    2
}

test async fn cancellation_cleanup() {
    let winner = Task.any(slow(), fast()).await
    assert winner == 2
}
",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open cancellation project")
        .snapshot()
        .expect("compile cancellation project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute cancellation test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn nested_control_await_executes_through_resume_dispatch() {
    let project = TestProject::new();
    project.write(
        "async.loom",
        "module sample\n\nasync fn child() Int { 1 }\n\nasync fn pending(flag Bool) Int {\n    if flag { child().await } else { 0 }\n}\n\ntest async fn nested_control() {\n    let value = pending(true).await\n    assert value == 1\n}\n",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open nested async project")
        .snapshot()
        .expect("analyze nested async project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute nested async test");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn result_propagation_rejects_invalid_operand_return_and_cleanup_shapes() {
    let project = TestProject::new();
    project.write(
        "propagation.loom",
        r"module sample

enum FirstProblem { Failed }
enum SecondProblem { Failed }

fn first() Result[Int, FirstProblem] {
    Err(FirstProblem.Failed)
}

fn invalidOperand() Result[Int, FirstProblem] {
    let value = 1?
    Ok(value)
}

fn invalidReturn() Int {
    first()?
}

fn invalidError() Result[Int, SecondProblem] {
    first()?
}

fn invalidCleanup() Result[Int, FirstProblem] {
    defer {
        let value = first()?
        Unit
    }
    first()
}
",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open propagation project")
        .snapshot()
        .expect("analyze propagation project");
    let codes = snapshot
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"PropagationRequiresResult"), "{codes:?}");
    assert!(
        codes.contains(&"PropagationRequiresResultReturn"),
        "{codes:?}"
    );
    assert!(codes.contains(&"PropagationErrorTypeMismatch"), "{codes:?}");
    assert!(codes.contains(&"PropagationInCleanup"), "{codes:?}");
    assert!(snapshot.executable().is_err());
}

#[test]
fn task_sleep_validates_consumption_argument_and_async_context() {
    let project = TestProject::new();
    project.write(
        "sleep.loom",
        r#"module sample

async fn unawaited() {
    let task = Task.sleep(1)
}

async fn wrongType() {
    Task.sleep("soon").await
}

async fn wrongArity() {
    Task.sleep().await
}

fn synchronous() {
    Task.sleep(1).await
}
"#,
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open sleep project")
        .snapshot()
        .expect("analyze sleep project");
    let codes = snapshot
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"UnawaitedAsyncCall"), "{codes:?}");
    assert!(codes.contains(&"TypeMismatch"), "{codes:?}");
    assert!(codes.contains(&"AwaitOutsideAsync"), "{codes:?}");
    assert!(snapshot.executable().is_err());
}

#[test]
fn nested_contract_bindings_execute_from_source() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module sample\n\nenum Problem { Failed }\n\nfn keep(value Option[Int]) Result[Option[Int], Problem]\n    ensures match result {\n        Ok(option) => match option {\n            Some(number) => number >= 0\n            None => true\n        }\n        Err(_) => true\n    }\n{\n    Ok(value)\n}\n\ntest fn checks_nested_binding() {\n    match keep(Some(3)) {\n        Ok(Some(number)) => {\n            assert number == 3\n            Unit\n        }\n        Ok(None) => {\n            assert false\n            Unit\n        }\n        Err(_) => {\n            assert false\n            Unit\n        }\n    }\n}\n",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open host")
        .snapshot()
        .expect("compile project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute checked MIR");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn conditional_conformance_proof_executes_from_source() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module sample\n\nconcept Equivalent {\n    method equivalent(self, other Self) Bool\n}\n\nrecord Atom { value Int }\n\nimpl Equivalent for Atom {\n    method equivalent(self, other Atom) Bool { self.value == other.value }\n}\n\nrecord Boxed[T] { value T }\n\nimpl[T: Equivalent] Equivalent for Boxed[T] {\n    method equivalent(self, other Boxed[T]) Bool {\n        self.value.equivalent(other.value)\n    }\n}\n\nfn same[T: Equivalent](left T, right T) Bool {\n    left.equivalent(right)\n}\n\ntest fn recursive_proof() {\n    let left = Boxed { value = Atom { value = 7 } }\n    let right = Boxed { value = Atom { value = 7 } }\n    let equal = same(left, right)\n    assert equal\n}\n",
    );
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open host")
        .snapshot()
        .expect("compile project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let results = snapshot.run_tests().expect("execute conditional proof");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, TestStatus::Passed, "{results:#?}");
}

#[test]
fn semantic_symbol_index_covers_generics_parameters_and_locals() {
    let project = TestProject::new();
    let source =
        "module sample\n\npub fn identity[T](value T) T {\n    let copy = value\n    copy\n}\n";
    project.write("main.loom", source);
    let snapshot = AnalysisHost::new(&project.root)
        .expect("open symbol project")
        .snapshot()
        .expect("analyze symbol project");
    assert!(!snapshot.has_errors(), "{:#?}", snapshot.diagnostics());
    let file = snapshot
        .sources()
        .documents()
        .iter()
        .find(|source| source.relative_path() == "main.loom")
        .expect("symbol test source")
        .id();

    let generic_declaration = u32::try_from(source.find("[T]").expect("generic declaration") + 1)
        .expect("source offset fits u32");
    let parameter_declaration =
        u32::try_from(source.find("value T").expect("parameter")).expect("source offset fits u32");
    let parameter_use = u32::try_from(source.rfind("value").expect("parameter use"))
        .expect("source offset fits u32");
    let local_declaration = u32::try_from(source.find("copy =").expect("local declaration"))
        .expect("source offset fits u32");
    let local_use =
        u32::try_from(source.rfind("copy").expect("local use")).expect("source offset fits u32");

    assert!(matches!(
        snapshot
            .definition_at(file, generic_declaration)
            .expect("generic symbol")
            .id,
        SymbolId::GenericParam(_)
    ));
    assert!(matches!(
        snapshot
            .definition_at(file, parameter_use)
            .expect("parameter symbol")
            .id,
        SymbolId::Param(_)
    ));
    assert_eq!(
        snapshot
            .references_at(file, parameter_declaration, true)
            .expect("parameter references")
            .len(),
        2
    );
    assert!(matches!(
        snapshot
            .definition_at(file, local_use)
            .expect("local symbol")
            .id,
        SymbolId::Local { .. }
    ));
    assert_eq!(
        snapshot
            .references_at(file, local_declaration, true)
            .expect("local references")
            .len(),
        2
    );

    let names = snapshot
        .document_symbols(file)
        .into_iter()
        .map(|symbol| symbol.name)
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "identity"), "{names:?}");
    assert!(names.iter().any(|name| name == "T"), "{names:?}");
    assert!(names.iter().any(|name| name == "value"), "{names:?}");
    assert!(names.iter().any(|name| name == "copy"), "{names:?}");

    let completions = snapshot
        .completion_symbols(file, local_use)
        .into_iter()
        .map(|symbol| symbol.name)
        .collect::<Vec<_>>();
    assert!(
        ["identity", "T", "value", "copy"]
            .iter()
            .all(|expected| completions.iter().any(|name| name == expected)),
        "{completions:?}"
    );
}
