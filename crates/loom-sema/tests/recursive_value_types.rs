use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{PackageSourceUnit, SourceUnit, lower_files, lower_package_files};
use loom_sema::analyze;
use loom_syntax::parse_with_file;

fn analyze_source(source: &str) -> Vec<loom_core::Diagnostic> {
    let parsed = parse_with_file(FileId(0), source);
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics: {:#?}",
        parsed.diagnostics()
    );
    let lowered = lower_files([SourceUnit {
        file: FileId(0),
        syntax: parsed.ast(),
    }]);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    analyze(&lowered.program).diagnostics
}

fn recursive_diagnostics(source: &str) -> Vec<loom_core::Diagnostic> {
    analyze_source(source)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == "RecursiveValueType")
        .collect()
}

fn analyze_source_with_canonical_json(source: &str) -> Vec<loom_core::Diagnostic> {
    let application_file = FileId(0);
    let json_file = FileId(1);
    let parsed = parse_with_file(application_file, source);
    let json = parse_with_file(
        json_file,
        r"
pub enum Json {
    Null
    Bool(Bool)
    Number(Float)
    Text(Text)
    Array(List[Json])
    Object(TextMap[Json])
}

pub enum JsonError {
    InvalidSyntax(Int)
    NumberOutOfRange(Int)
    DepthLimit
    NonFiniteNumber
}
",
    );
    assert!(
        parsed.diagnostics().is_empty() && json.diagnostics().is_empty(),
        "syntax diagnostics: application={:#?} json={:#?}",
        parsed.diagnostics(),
        json.diagnostics()
    );

    let application = PackageId::new("recursive-value-types", "0");
    let std = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: application_file,
            package: application.clone(),
            module: ModuleName::new("recursive_value_types"),
            syntax: parsed.ast(),
        },
        PackageSourceUnit {
            file: json_file,
            package: std.clone(),
            module: ModuleName::new("std.json"),
            syntax: json.ast(),
        },
    ]);
    assert!(
        lowered.diagnostics.is_empty(),
        "HIR diagnostics: {:#?}",
        lowered.diagnostics
    );
    lowered.program.register_package(std.clone(), [], false);
    lowered
        .program
        .register_package(application, [(Name::new("std"), std)], true);
    analyze(&lowered.program).diagnostics
}

#[test]
fn rejects_direct_record_enum_refinement_and_option_cycles() {
    let diagnostics = recursive_diagnostics(
        r"
record RecordLoop {
    next RecordLoop
}

enum EnumLoop {
    Again(EnumLoop)
}

type RefinedLoop = RefinedLoop where true

record OptionLoop {
    next Option[OptionLoop]
}
",
    );

    assert_eq!(diagnostics.len(), 4, "{diagnostics:#?}");
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        [
            "by-value nominal type `RecordLoop` has infinite size because it contains itself",
            "by-value nominal type `EnumLoop` has infinite size because it contains itself",
            "by-value nominal type `RefinedLoop` has infinite size because it contains itself",
            "by-value nominal type `OptionLoop` has infinite size because it contains itself",
        ]
    );
}

#[test]
fn rejects_one_mutual_cycle_across_nominal_kinds() {
    let diagnostics = recursive_diagnostics(
        r"
record First {
    next Second
}

enum Second {
    More(Third)
}

type Third = First where true
",
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(
        diagnostics[0].message,
        "by-value nominal types `First`, `Second`, `Third` form an infinite-size cycle"
    );
    assert_eq!(diagnostics[0].labels.len(), 2);
}

#[test]
fn rejects_regular_non_regular_and_nominal_argument_cycles() {
    let diagnostics = recursive_diagnostics(
        r"
record Regular[T] {
    next Regular[T]
}

record NonRegular[T] {
    next NonRegular[(T, T)]
}

record Phantom[T] {
    marker Int
}

record ThroughArgument {
    value Phantom[ThroughArgument]
}
",
    );

    assert_eq!(diagnostics.len(), 3, "{diagnostics:#?}");
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        [
            "by-value nominal type `Regular` has infinite size because it contains itself",
            "by-value nominal type `NonRegular` has infinite size because it contains itself",
            "by-value nominal type `ThroughArgument` has infinite size because it contains itself",
        ]
    );
}

#[test]
fn indirect_carriers_break_cycles_and_canonical_json_remains_valid() {
    let diagnostics = analyze_source_with_canonical_json(
        r"
dyn concept Link {
    associated type Target
}

record ListNode {
    next List[ListNode]
}

record MapNode {
    next TextMap[MapNode]
}

record TaskNode {
    next Task[TaskNode]
}

record ViewNode {
    next dyn Link[Target = ViewNode]
}

record JsonHolder {
    value Json
}

type CheckedJson = Json where true
",
    );

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
}
