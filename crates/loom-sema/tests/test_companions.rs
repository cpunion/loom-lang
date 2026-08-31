use loom_core::{FileId, ModuleName, PackageId};
use loom_hir::{
    DefinitionKind, ModuleResolution, PackageSourceMode, PackageSourceUnit,
    SelectedPackageSourceUnit, lower_selected_package_files,
};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn source<'a>(
    file: FileId,
    package: &PackageId,
    module: &str,
    parse: &'a loom_syntax::Parse,
    mode: PackageSourceMode,
) -> SelectedPackageSourceUnit<'a> {
    SelectedPackageSourceUnit {
        source: PackageSourceUnit {
            file,
            package: package.clone(),
            module: ModuleName::new(module),
            syntax: parse.ast(),
        },
        mode,
    }
}

#[test]
fn companion_tests_and_helpers_share_one_way_private_access() {
    let package = PackageId::standalone();
    let production = parse_with_file(
        FileId(0),
        r"fn secret() Int { 42 }

fn production() Int { secret() }

test fn embedded() {
    let productionValue = secret()
    let testValue = helper()
    assert productionValue == testValue
}
",
    );
    let tests = parse_with_file(
        FileId(1),
        r"fn helper() Int { secret() }

test fn fromFile() {
    let value = helper()
    assert value == 42
}
",
    );
    assert!(production.diagnostics().is_empty());
    assert!(tests.diagnostics().is_empty());

    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "standalone",
            &production,
            PackageSourceMode::ProductionAndTests,
        ),
        source(
            FileId(1),
            &package,
            "standalone",
            &tests,
            PackageSourceMode::TestCompanion,
        ),
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );

    let production_module = lowered
        .program
        .module_by_name(&ModuleName::new("standalone"))
        .expect("production module");
    let companion = lowered
        .program
        .test_companion(production_module)
        .expect("test companion");
    assert!(
        lowered
            .program
            .can_access_private(companion, production_module)
    );
    assert!(
        !lowered
            .program
            .can_access_private(production_module, companion)
    );
    assert_eq!(
        lowered.program.modules[production_module].files,
        [FileId(0)]
    );
    assert_eq!(
        lowered.program.modules[companion].files,
        [FileId(0), FileId(1)]
    );
    assert_eq!(
        lowered
            .program
            .definitions
            .iter()
            .filter(|(_, definition)| matches!(definition.kind, DefinitionKind::Test(_)))
            .count(),
        2
    );
}

#[test]
fn companion_can_use_a_production_private_type_and_inherent_method() {
    let package = PackageId::new("application", "1.0.0");
    let production = parse_with_file(
        FileId(0),
        r"record Counter {
    value Int
}

impl Counter {
    method secret(self) Int { self.value }
}
",
    );
    let tests = parse_with_file(
        FileId(1),
        r"test fn readsPrivateMethod() {
    let counter = Counter { value = 42 }
    let value = counter.secret()
    assert value == 42
}
",
    );
    assert!(production.diagnostics().is_empty());
    assert!(tests.diagnostics().is_empty());

    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "application.library",
            &production,
            PackageSourceMode::Production,
        ),
        source(
            FileId(1),
            &package,
            "application.library",
            &tests,
            PackageSourceMode::TestCompanion,
        ),
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn sibling_module_cannot_call_a_private_inherent_method() {
    let package = PackageId::new("application", "1.0.0");
    let library = parse_with_file(
        FileId(0),
        r"pub record Counter {
    value Int
}

impl Counter {
    method secret(self) Int { self.value }
}
",
    );
    let consumer = parse_with_file(
        FileId(1),
        r"import application.library.Counter

fn expose(value Counter) Int { value.secret() }
",
    );
    assert!(library.diagnostics().is_empty());
    assert!(consumer.diagnostics().is_empty());

    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "application.library",
            &library,
            PackageSourceMode::Production,
        ),
        source(
            FileId(1),
            &package,
            "application.consumer",
            &consumer,
            PackageSourceMode::Production,
        ),
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "UnknownName"
                && diagnostic.primary.file == FileId(1)
                && diagnostic.message.contains("no method `secret`")
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn production_bodies_cannot_resolve_companion_helpers() {
    let package = PackageId::standalone();
    let production = parse_with_file(
        FileId(0),
        "fn production() Int { helper() }\n\ntest fn embedded() {}\n",
    );
    let tests = parse_with_file(FileId(1), "fn helper() Int { 42 }\n");
    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "standalone",
            &production,
            PackageSourceMode::ProductionAndTests,
        ),
        source(
            FileId(1),
            &package,
            "standalone",
            &tests,
            PackageSourceMode::TestCompanion,
        ),
    ]);
    let analysis = analyze(&lowered.program);
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "UnknownName"),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn test_only_impls_cannot_extend_production_types_or_leak_into_production_bodies() {
    let package = PackageId::standalone();
    let production = parse_with_file(
        FileId(0),
        r"record Counter {
    value Int
}

concept Readable {
    method read(self) Int
}

fn readValue[T: Readable](value T) Int { value.read() }

fn throughTestMethod(value Counter) Int { value.testOnly() }

fn throughTestConformance(value Counter) Int { readValue(value) }

test fn embedded() {}
",
    );
    let tests = parse_with_file(
        FileId(1),
        r"impl Counter {
    method testOnly(self) Int { self.value }
}

impl Readable for Counter {
    method read(self) Int { self.value }
}
",
    );
    assert!(production.diagnostics().is_empty());
    assert!(tests.diagnostics().is_empty());

    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "standalone",
            &production,
            PackageSourceMode::ProductionAndTests,
        ),
        source(
            FileId(1),
            &package,
            "standalone",
            &tests,
            PackageSourceMode::TestCompanion,
        ),
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    let analysis = analyze(&lowered.program);

    for expected in [
        "ForeignInherentImpl",
        "ForeignConformance",
        "UnknownName",
        "MissingConformance",
    ] {
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == expected),
            "missing {expected}: {:#?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn production_projection_drops_test_bodies_before_hir() {
    let package = PackageId::standalone();
    let production = parse_with_file(
        FileId(0),
        "fn valid() Int { 42 }\n\ntest fn ignored() { missing() }\n",
    );
    let lowered = lower_selected_package_files([source(
        FileId(0),
        &package,
        "standalone",
        &production,
        PackageSourceMode::Production,
    )]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    assert!(
        lowered
            .program
            .definitions
            .iter()
            .all(|(_, definition)| !matches!(definition.kind, DefinitionKind::Test(_)))
    );
    assert!(analyze(&lowered.program).diagnostics.is_empty());
}

#[test]
fn a_real_test_directory_cannot_collide_with_companion_authority() {
    let package = PackageId::new("application", "1.0.0");
    let root = parse_with_file(FileId(0), "fn privateRoot() {}\n\ntest fn selected() {}\n");
    let real_test_directory = parse_with_file(FileId(1), "fn privateDirectory() {}\n");
    let lowered = lower_selected_package_files([
        source(
            FileId(0),
            &package,
            "application",
            &root,
            PackageSourceMode::ProductionAndTests,
        ),
        source(
            FileId(1),
            &package,
            "application.test",
            &real_test_directory,
            PackageSourceMode::Production,
        ),
    ]);
    let root = lowered
        .program
        .module_by_name(&ModuleName::new("application"))
        .expect("root production module");
    let real = lowered
        .program
        .module_by_name(&ModuleName::new("application.test"))
        .expect("real test-directory module");
    let companion = lowered.program.test_companion(root).expect("companion");

    assert_ne!(real, companion);
    assert!(lowered.program.is_production_module(real));
    assert!(!lowered.program.can_access_private(real, root));
    assert!(!lowered.program.can_access_private(companion, real));
    assert_eq!(
        lowered
            .program
            .resolve_module_from(companion, &ModuleName::new("application.test")),
        ModuleResolution::Found(real)
    );
}
