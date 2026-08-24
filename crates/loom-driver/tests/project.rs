use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use loom_core::FileId;
use loom_driver::{
    AnalysisHost, CacheContext, CacheLookup, PersistentCache, PipelineStage, Position, TargetKind,
    discover_loom_files, format_source,
};
use loom_hir::{SourceUnit, lower_files};
use loom_interpreter::TestStatus;
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
        compiler_version: "test-compiler-v1".to_owned(),
        backend_version: "test-backend-v1".to_owned(),
        standard_library_version: "test-stdlib-v1".to_owned(),
        runtime_abi_version: "test-runtime-v1".to_owned(),
        target_triple: "portable-test".to_owned(),
        data_layout: "value-v1".to_owned(),
        cpu_policy: "portable".to_owned(),
        optimization: "none".to_owned(),
        contract_mode: "checked".to_owned(),
    }
}

#[test]
fn first_class_dynamic_values_execute_in_the_interpreter() {
    let project = TestProject::new();
    project.write(
        "concepts.loom",
        include_str!("../../../examples/core02/concepts.loom"),
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
        include_str!("../../../examples/core03/tasks.loom"),
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
        "schema = 1\n\n[package]\nname = \"application\"\nversion = \"0.1.0\"\nsources = [\"src\"]\n\n[dependencies]\nutility = { path = \"../utility\", version = \"^1.0\" }\n\n[[target]]\nname = \"app\"\nkind = \"bin\"\nentry = \"application.start\"\n\n[[target]]\nname = \"unit\"\nkind = \"test\"\n",
    );
    project.write(
        "application/src/main.loom",
        "module application\n\nimport utility.math.increment\n\npub fn start() Unit {\n    let value = increment(1)\n    assert value == 2\n    Unit\n}\n\ntest fn dependency_works() {\n    let value = increment(4)\n    assert value == 5\n    Unit\n}\n",
    );

    let host = AnalysisHost::new(project.root.join("application")).expect("open manifest project");
    assert!(host.project().manifest().is_some());
    assert_eq!(host.project().packages().count(), 2);
    let target = host.project().target("app").expect("binary target");
    assert_eq!(target.kind(), TargetKind::Bin);
    assert_eq!(target.entry(), Some("application.start"));

    let snapshot = host.snapshot().expect("compile package graph");
    let paths = snapshot
        .sources()
        .documents()
        .iter()
        .map(loom_driver::SourceDocument::relative_path)
        .collect::<Vec<_>>();
    assert_eq!(paths, ["deps/utility@1.2.0/src/math.loom", "src/main.loom"]);
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
fn persistent_cache_is_relocatable_and_corruption_is_a_safe_miss() {
    let first = TestProject::new();
    let second = TestProject::new();
    for project in [&first, &second] {
        project.write(
            "loom.toml",
            "schema = 1\n[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
        );
        project.write(
            "src/main.loom",
            "module sample\n\npub fn main() Unit {\n    Unit\n}\n",
        );
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
        "module sample\n\npub fn main() Unit {\n    assert true\n    Unit\n}\n",
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
    project.write(
        "main.loom",
        "module sample\n\npub fn main() Unit {\n    Unit\n}\n",
    );
    let host = AnalysisHost::new(&project.root).expect("open cached parse project");
    let cache = PersistentCache::new(project.root.join("target/parse-cache"));
    let context = portable_cache_context();

    let (first, first_stats) = host.snapshot_from_sources_with_parse_cache(
        host.load_sources().expect("load first source set"),
        &cache,
        &context.compiler_version,
    );
    assert!(!first.has_errors(), "{:?}", first.diagnostics());
    assert_eq!(first_stats.hits, 0);
    assert_eq!(first_stats.misses, 1);

    let (second, second_stats) = host.snapshot_from_sources_with_parse_cache(
        host.load_sources().expect("load second source set"),
        &cache,
        &context.compiler_version,
    );
    assert!(!second.has_errors(), "{:?}", second.diagnostics());
    assert!(second_stats.is_full_hit());
    assert_eq!(second_stats.hits, 1);
    assert_eq!(
        first.parse(FileId(0)).expect("first parse").ast(),
        second.parse(FileId(0)).expect("cached parse").ast()
    );
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
fn concurrent_cache_writers_publish_only_complete_blobs_and_refs() {
    let project = TestProject::new();
    project.write(
        "main.loom",
        "module sample\n\npub fn main() Unit {\n    Unit\n}\n",
    );
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
            "module stress.app\n\nimport stress.m0.f0\n\npub fn main() Unit {{\n    let answer = f0(0)\n    assert answer == {MODULES_MINUS_ONE}\n    Unit\n}}\n",
            MODULES_MINUS_ONE = MODULES - 1
        ),
    );

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open scale project")
        .snapshot()
        .expect("compile scale project");
    assert!(!snapshot.has_errors(), "{:?}", snapshot.diagnostics());
    let program = snapshot.executable().expect("scale project checked MIR");
    assert_eq!(program.functions.len(), MODULES + 1);
    assert!(program.exports.contains_key("stress.app.main"));
}

#[test]
fn snapshot_assigns_file_ids_by_stable_relative_path_and_builds_executable_mir() {
    let project = TestProject::new();
    project.write("b.loom", "module b\n\nfn b() Unit {\n    Unit\n}\n");
    project.write("a.loom", "module a\n\nfn a() Unit {\n    Unit\n}\n");

    let snapshot = AnalysisHost::new(&project.root)
        .expect("open host")
        .snapshot()
        .expect("build snapshot");
    assert_eq!(snapshot.sources().documents()[0].relative_path(), "a.loom");
    assert_eq!(snapshot.sources().documents()[0].id(), FileId(0));
    assert_eq!(snapshot.sources().documents()[1].relative_path(), "b.loom");
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
    project.write(
        "main.loom",
        "module demo\n\nfn main() Unit {\n    Unit\n}\n",
    );
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
    assert_eq!(dirty.sources().file_id(&path), Some(FileId(0)));

    host.clear_overlay(&path).expect("clear overlay");
    let clean = host.snapshot().expect("clean snapshot");
    assert!(!clean.has_errors(), "{:?}", clean.diagnostics());
    clean
        .executable()
        .expect("clearing the overlay restores executable MIR");
}

#[test]
fn formatter_is_canonical_idempotent_and_refuses_broken_source() {
    let source = "module demo\r\n\r\nfn main() Unit {\r\n\tUnit   \r\n}\r\n\r\n";
    let first = format_source(FileId(0), source);
    assert!(first.diagnostics.is_empty());
    assert_eq!(first.text, "module demo\n\nfn main() Unit {\n    Unit\n}\n");
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
    let source = &snapshot.sources().documents()[0];
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
        r#"module standard.resource

concept Dispose {
    method dispose(mut self) Unit
}

concept MustScope {}
concept NoSuspend {}

record Resource {
    value Int
}

impl Dispose for Resource {
    method dispose(mut self) Unit {
        self.value = 0
        Unit
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
    Unit
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
    Task.sleep(1).await
    2
}

test async fn stored_and_dynamic_joins() {
    Task.waitWritable(1).await
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
    Unit
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
    Unit
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
        r#"module standard_io

import standard.time.milliseconds
import standard.file.open_read
import standard.file.create
import standard.net.connect

test async fn real_io() {{
    let delay = milliseconds(1)
    let observed = delay.as_milliseconds()
    assert observed == 1
    Task.sleep(delay).await
    {{
        scoped output = create("{}").await
        output.write_text("hello from loom").await
        Unit
    }}
    {{
        scoped input = open_read("{}").await
        let content = input.read_text().await
        assert content == "hello from loom"
        Unit
    }}
    {{
        scoped socket = connect("127.0.0.1", {}).await
        socket.write_text("ping").await
        let response = socket.read_text().await
        assert response == "pong"
        Unit
    }}
    Unit
}}
"#,
        file.display(),
        file.display(),
        port,
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

import standard.file.open_read
import standard.net.connect

async fn leakedFile() Unit {
    let file = open_read("missing.txt").await
    Unit
}

async fn discardedSocket() Unit {
    connect("127.0.0.1", 1).await
    Unit
}

async fn closedTwice() Unit {
    scoped file = open_read("missing.txt").await
    file.close()
    Unit
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
    Task.sleep(1).await
    2
}

test async fn cancellation_cleanup() {
    let winner = Task.any(slow(), fast()).await
    assert winner == 2
    Unit
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
        "module sample\n\nasync fn child() Int { 1 }\n\nasync fn pending(flag Bool) Int {\n    if flag { child().await } else { 0 }\n}\n\ntest async fn nested_control() {\n    let value = pending(true).await\n    assert value == 1\n    Unit\n}\n",
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

async fn unawaited() Unit {
    let task = Task.sleep(1)
    Unit
}

async fn wrongType() Unit {
    Task.sleep("soon").await
}

async fn wrongArity() Unit {
    Task.sleep().await
}

fn synchronous() Unit {
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
        "module sample\n\nconcept Equivalent {\n    method equivalent(self, other Self) Bool\n}\n\nrecord Atom { value Int }\n\nimpl Equivalent for Atom {\n    method equivalent(self, other Atom) Bool { self.value == other.value }\n}\n\nrecord Boxed[T] { value T }\n\nimpl[T: Equivalent] Equivalent for Boxed[T] {\n    method equivalent(self, other Boxed[T]) Bool {\n        self.value.equivalent(other.value)\n    }\n}\n\nfn same[T: Equivalent](left T, right T) Bool {\n    left.equivalent(right)\n}\n\ntest fn recursive_proof() {\n    let left = Boxed { value = Atom { value = 7 } }\n    let right = Boxed { value = Atom { value = 7 } }\n    let equal = same(left, right)\n    assert equal\n    Unit\n}\n",
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
