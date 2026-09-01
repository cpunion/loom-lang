use std::collections::BTreeMap;

use loom_core::{FileId, LOOM_LANGUAGE_VERSION, ModuleName, Name, PackageId};
use loom_hir::{DefId, DefinitionKind, PackageSourceUnit, Program, lower_package_files};
use loom_sema::{Analysis, BuiltinValue, CallTarget, Signature, TyData, analyze};
use loom_syntax::parse_with_file;

const FILE_SOURCE: &str = include_str!("../../../library/std/file/file.loom");
const NET_SOURCE: &str = include_str!("../../../library/std/net/net.loom");
const IO_SOURCE: &str = include_str!("../../../library/std/io/io.loom");
const PATH_SOURCE: &str = include_str!("../../../library/std/path/path.loom");
const RESOURCE_SOURCE: &str = include_str!("../../../library/std/resource/resource.loom");

const PRIVATE_BUILTINS: [BuiltinValue; 6] = [
    BuiltinValue::FileOpenRead,
    BuiltinValue::FileCreate,
    BuiltinValue::FileTryOpenRead,
    BuiltinValue::FileTryCreate,
    BuiltinValue::SocketConnect,
    BuiltinValue::SocketTryConnect,
];

fn definition_named(program: &Program, package: &PackageId, module: &str, name: &str) -> DefId {
    program
        .definitions
        .iter()
        .find_map(|(definition, item)| {
            let owner = &program.modules[item.module];
            (owner.package == *package
                && owner.name.as_str() == module
                && item
                    .name
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == name))
            .then_some(definition)
        })
        .unwrap_or_else(|| panic!("missing {package:?} {module}.{name}"))
}

fn call_targets(program: &Program, analysis: &Analysis, definition: DefId) -> Vec<CallTarget> {
    let body = match &program.definitions[definition].kind {
        DefinitionKind::Function(function) => function.body,
        DefinitionKind::Method(method) => method.body.expect("source method body"),
        _ => panic!("definition is not callable"),
    };
    analysis
        .typed
        .body(body)
        .expect("checked callable body")
        .calls
        .values()
        .map(|call| call.target.clone())
        .collect()
}

fn assert_task_result_error(analysis: &Analysis, definition: DefId, expected_error: DefId) {
    let Some(Signature::Callable(signature)) = analysis.typed.signatures.get(definition) else {
        panic!("definition is not callable")
    };
    let TyData::Task(output) = analysis.typed.types.data(signature.return_ty) else {
        panic!("callable does not return Task")
    };
    let TyData::Result { error, .. } = analysis.typed.types.data(*output) else {
        panic!("Task does not contain Result")
    };
    assert_eq!(
        analysis.typed.types.data(*error),
        &TyData::Nominal {
            definition: expected_error,
            arguments: Vec::new(),
        }
    );
}

fn parse(file: FileId, source: &str) -> loom_syntax::Parse {
    let parsed = parse_with_file(file, source);
    assert!(
        parsed.diagnostics().is_empty(),
        "syntax diagnostics for file {}: {:#?}",
        file.0,
        parsed.diagnostics()
    );
    parsed
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one vertical boundary test keeps all public wrappers and private I/O owners together"
)]
fn public_file_and_net_calls_are_source_functions_with_exact_private_owners() {
    let io_file = FileId(0);
    let file_file = FileId(1);
    let net_file = FileId(2);
    let resource_file = FileId(3);
    let path_file = FileId(4);
    let application_file = FileId(5);
    let io = parse(io_file, IO_SOURCE);
    let file = parse(file_file, FILE_SOURCE);
    let net = parse(net_file, NET_SOURCE);
    let resource = parse(resource_file, RESOURCE_SOURCE);
    let path = parse(path_file, PATH_SOURCE);
    let application = parse(
        application_file,
        r"
import std.file.open_read
import std.file.create
import std.file.open_read_path
import std.file.create_path
import std.file.try_open_read
import std.file.try_create
import std.file.try_open_read_path
import std.file.try_create_path
import std.net.connect
import std.net.try_connect

pub fn app_open_read(path Text) Task[File] { open_read(path) }
pub fn app_create(path Text) Task[File] { create(path) }
pub fn app_open_read_path(path Path) Task[File] { open_read_path(path) }
pub fn app_create_path(path Path) Task[File] { create_path(path) }
pub fn app_try_open_read(path Text) Task[Result[File, IoError]] { try_open_read(path) }
pub fn app_try_create(path Text) Task[Result[File, IoError]] { try_create(path) }
pub fn app_try_open_read_path(path Path) Task[Result[File, IoError]] {
    try_open_read_path(path)
}
pub fn app_try_create_path(path Path) Task[Result[File, IoError]] {
    try_create_path(path)
}
pub fn app_connect(host Text, port Int) Task[Socket] { connect(host, port) }
pub fn app_try_connect(host Text, port Int) Task[Result[Socket, IoError]] {
    try_connect(host, port)
}
",
    );

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: io_file,
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: io.ast(),
        },
        PackageSourceUnit {
            file: file_file,
            package: std_package.clone(),
            module: ModuleName::new("std.file"),
            syntax: file.ast(),
        },
        PackageSourceUnit {
            file: net_file,
            package: std_package.clone(),
            module: ModuleName::new("std.net"),
            syntax: net.ast(),
        },
        PackageSourceUnit {
            file: resource_file,
            package: std_package.clone(),
            module: ModuleName::new("std.resource"),
            syntax: resource.ast(),
        },
        PackageSourceUnit {
            file: path_file,
            package: std_package.clone(),
            module: ModuleName::new("std.path"),
            syntax: path.ast(),
        },
        PackageSourceUnit {
            file: application_file,
            package: application_package.clone(),
            module: ModuleName::new("application"),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    lowered.program.register_package(
        application_package.clone(),
        [(Name::new("std"), std_package.clone())],
        true,
    );

    let analysis = analyze(&lowered.program);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    let io_error = definition_named(&lowered.program, &std_package, "std.io", "IoError");
    assert_eq!(analysis.canonical_std_items.io_error, Some(io_error));
    let DefinitionKind::Record(record) = &lowered.program.definitions[io_error].kind else {
        panic!("canonical IoError must be an ordinary source record")
    };
    assert_eq!(record.fields.len(), 2, "IoError fields are source-declared");

    let public_functions = [
        ("std.file", "open_read", "app_open_read"),
        ("std.file", "create", "app_create"),
        ("std.file", "open_read_path", "app_open_read_path"),
        ("std.file", "create_path", "app_create_path"),
        ("std.file", "try_open_read", "app_try_open_read"),
        ("std.file", "try_create", "app_try_create"),
        ("std.file", "try_open_read_path", "app_try_open_read_path"),
        ("std.file", "try_create_path", "app_try_create_path"),
        ("std.net", "connect", "app_connect"),
        ("std.net", "try_connect", "app_try_connect"),
    ];
    let definitions = public_functions
        .iter()
        .map(|(module, public_name, application_name)| {
            let public = definition_named(&lowered.program, &std_package, module, public_name);
            let application = definition_named(
                &lowered.program,
                &application_package,
                "application",
                application_name,
            );
            assert_eq!(
                call_targets(&lowered.program, &analysis, application),
                vec![CallTarget::Function(public)],
                "application call must target the ordinary source wrapper"
            );
            if public_name.starts_with("try_") {
                assert_task_result_error(&analysis, public, io_error);
                assert_task_result_error(&analysis, application, io_error);
            }
            ((*module, *public_name), public)
        })
        .collect::<BTreeMap<_, _>>();

    let open_read = definitions[&("std.file", "open_read")];
    let create = definitions[&("std.file", "create")];
    let try_open_read = definitions[&("std.file", "try_open_read")];
    let try_create = definitions[&("std.file", "try_create")];
    let connect = definitions[&("std.net", "connect")];
    let try_connect = definitions[&("std.net", "try_connect")];
    let path_as_text = definition_named(&lowered.program, &std_package, "std.path", "as_text");
    for (definition, builtin) in [
        (open_read, BuiltinValue::FileOpenRead),
        (create, BuiltinValue::FileCreate),
        (try_open_read, BuiltinValue::FileTryOpenRead),
        (try_create, BuiltinValue::FileTryCreate),
        (connect, BuiltinValue::SocketConnect),
        (try_connect, BuiltinValue::SocketTryConnect),
    ] {
        assert_eq!(
            call_targets(&lowered.program, &analysis, definition),
            vec![CallTarget::Builtin(builtin)],
            "only the exact source wrapper may own {builtin:?}"
        );
    }

    for (path_wrapper, text_wrapper) in [
        ("open_read_path", open_read),
        ("create_path", create),
        ("try_open_read_path", try_open_read),
        ("try_create_path", try_create),
    ] {
        let definition = definitions[&("std.file", path_wrapper)];
        let targets = call_targets(&lowered.program, &analysis, definition);
        assert!(
            targets.contains(&CallTarget::InherentMethod(path_as_text)),
            "{path_wrapper} must lower Path through its public Text spelling: {targets:#?}"
        );
        assert!(
            targets.contains(&CallTarget::Function(text_wrapper)),
            "{path_wrapper} must call the public Text wrapper: {targets:#?}"
        );
        assert!(
            targets.iter().all(|target| !matches!(
                target,
                CallTarget::Builtin(builtin) if PRIVATE_BUILTINS.contains(builtin)
            )),
            "Path wrappers must not own a private acquisition primitive: {targets:#?}"
        );
    }
    assert_eq!(
        call_targets(&lowered.program, &analysis, path_as_text),
        [CallTarget::Builtin(BuiltinValue::PathAsText)],
        "only the exact Path.as_text source method may own its private leaf"
    );

    let expected_owners = [
        (BuiltinValue::FileOpenRead, open_read),
        (BuiltinValue::FileCreate, create),
        (BuiltinValue::FileTryOpenRead, try_open_read),
        (BuiltinValue::FileTryCreate, try_create),
        (BuiltinValue::SocketConnect, connect),
        (BuiltinValue::SocketTryConnect, try_connect),
    ];
    for (definition, item) in lowered.program.definitions.iter() {
        let DefinitionKind::Function(function) = &item.kind else {
            continue;
        };
        let Some(body) = analysis.typed.body(function.body) else {
            continue;
        };
        for call in body.calls.values() {
            let CallTarget::Builtin(builtin) = call.target else {
                continue;
            };
            let Some((_, expected)) = expected_owners
                .iter()
                .find(|(candidate, _)| *candidate == builtin)
            else {
                continue;
            };
            assert_eq!(
                definition, *expected,
                "{builtin:?} escaped its exact source wrapper"
            );
        }
    }
}

const HOSTILE_SOURCE: &str = r"
import std.file.__open_read
import std.file.__create
import std.file.__try_open_read
import std.file.__try_create
import std.net.__connect
import std.net.__try_connect

fn private_open_read(path Text) Task[File] { __open_read(path) }
fn private_create(path Text) Task[File] { __create(path) }
fn private_try_open_read(path Text) Task[Result[File, IoError]] { __try_open_read(path) }
fn private_try_create(path Text) Task[Result[File, IoError]] { __try_create(path) }
fn private_connect(host Text, port Int) Task[Socket] { __connect(host, port) }
fn private_try_connect(host Text, port Int) Task[Result[Socket, IoError]] {
    __try_connect(host, port)
}
";

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the authority test assembles canonical, hostile-package, and hostile-module sources"
)]
fn private_file_and_net_primitives_reject_wrong_owner_package_and_application() {
    let io_file = FileId(0);
    let file_file = FileId(1);
    let net_file = FileId(2);
    let resource_file = FileId(3);
    let path_file = FileId(4);
    let wrong_owner_file = FileId(5);
    let wrong_package_file = FileId(6);
    let application_file = FileId(7);
    let io = parse(io_file, IO_SOURCE);
    let file = parse(file_file, FILE_SOURCE);
    let net = parse(net_file, NET_SOURCE);
    let resource = parse(resource_file, RESOURCE_SOURCE);
    let path = parse(path_file, PATH_SOURCE);
    let wrong_owner = parse(wrong_owner_file, HOSTILE_SOURCE);
    let wrong_package = parse(wrong_package_file, HOSTILE_SOURCE);
    let application = parse(application_file, HOSTILE_SOURCE);

    let std_package = PackageId::compiler_std(LOOM_LANGUAGE_VERSION);
    let hostile_package = PackageId::new("hostile-std", "0");
    let application_package = PackageId::new("application", "0");
    let mut lowered = lower_package_files([
        PackageSourceUnit {
            file: io_file,
            package: std_package.clone(),
            module: ModuleName::new("std.io"),
            syntax: io.ast(),
        },
        PackageSourceUnit {
            file: file_file,
            package: std_package.clone(),
            module: ModuleName::new("std.file"),
            syntax: file.ast(),
        },
        PackageSourceUnit {
            file: net_file,
            package: std_package.clone(),
            module: ModuleName::new("std.net"),
            syntax: net.ast(),
        },
        PackageSourceUnit {
            file: resource_file,
            package: std_package.clone(),
            module: ModuleName::new("std.resource"),
            syntax: resource.ast(),
        },
        PackageSourceUnit {
            file: path_file,
            package: std_package.clone(),
            module: ModuleName::new("std.path"),
            syntax: path.ast(),
        },
        PackageSourceUnit {
            file: wrong_owner_file,
            package: std_package.clone(),
            module: ModuleName::new("std.other"),
            syntax: wrong_owner.ast(),
        },
        PackageSourceUnit {
            file: wrong_package_file,
            package: hostile_package.clone(),
            module: ModuleName::new("std.file"),
            syntax: wrong_package.ast(),
        },
        PackageSourceUnit {
            file: application_file,
            package: application_package.clone(),
            module: ModuleName::new("application"),
            syntax: application.ast(),
        },
    ]);
    assert!(lowered.diagnostics.is_empty(), "{:#?}", lowered.diagnostics);
    lowered
        .program
        .register_package(std_package.clone(), [], false);
    lowered
        .program
        .register_package(hostile_package.clone(), [], false);
    lowered.program.register_package(
        application_package.clone(),
        [(Name::new("std"), std_package)],
        true,
    );

    let analysis = analyze(&lowered.program);
    for file in [wrong_owner_file, wrong_package_file, application_file] {
        assert!(
            analysis.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "UnknownName" && diagnostic.primary.file == file
            }),
            "missing private primitive rejection for file {}: {:#?}",
            file.0,
            analysis.diagnostics
        );
    }

    for (definition, item) in lowered.program.definitions.iter() {
        if !lowered.program.modules[item.module]
            .files
            .iter()
            .any(|file| [wrong_owner_file, wrong_package_file, application_file].contains(file))
        {
            continue;
        }
        let DefinitionKind::Function(function) = &item.kind else {
            continue;
        };
        let Some(body) = analysis.typed.body(function.body) else {
            continue;
        };
        assert!(
            body.calls.values().all(|call| !matches!(
                &call.target,
                CallTarget::Builtin(builtin) if PRIVATE_BUILTINS.contains(builtin)
            )),
            "unauthorized definition {definition:?} received a private file/net primitive"
        );
    }
}
