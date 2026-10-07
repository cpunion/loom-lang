use std::{fs, process::Command};
mod common;
use common::success;

const RESTORE: &str = r#"import std.list.get
import std.list.set
import std.list.length

fn restored(values List[Int], observed Int, changed Int)
requires observed >= 0 && observed < length(values)
requires changed >= 0 && changed < length(values)
ensures get(values, observed) == old(get(values, observed))
{
    let saved = get(values, changed)
    set(values, changed, 7)
    set(values, changed, saved)
}
"#;

#[test]
fn signed_bit_contracts_recheck_word_algebra_and_independent_operand_domains() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("bits.loom");
    let program = include_str!("../examples/smt_contracts/bitwise.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("mask <= 255", "mask <= 256"),
        program.replace("result <= 63", "result <= 62"),
        program.replace("(left ^ right) ^ right", "(left ^ right) ^ left"),
        program.replace("~(left & right)", "~(left | right)"),
        program.replace("count >= 0 && count < 64", "count >= 0"),
        program.replace("count >= 0 && count < 64", "count < 64"),
        program.replace("count < 64", "count <= 64"),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound bit proof: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    for unsafe_source in [
        "fn hidden(value Int) Bool\nensures ((value + 1) & 0) == 0\n{ true }\n",
        "fn hidden(value Int) Bool\nensures ((value + 1) ^ (value + 1)) == 0\n{ true }\n",
        "fn hidden(value Int) Bool\nensures (value << -1) == (value << -1)\n{ true }\n",
        "fn hidden(value Int) Bool\nensures (value >> 64) == (value >> 64)\n{ true }\n",
    ] {
        fs::write(&source, unsafe_source).unwrap();
        assert!(
            !check().status.success(),
            "unsafe bit operands: {unsafe_source}"
        );
    }
    fs::write(
        &source,
        format!("{program}\nfn main() {{\n    bitwise_cases()\n}}\n"),
    )
    .unwrap();
    for mode in ["test", "run"] {
        success(&common::loom(&[mode, package.path().to_str().unwrap()]));
    }
}

#[test]
fn immutable_text_bytes_recheck_ranges_utf8_and_hypothetical_access_domains() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("bytes.loom");
    let program = include_str!("../examples/smt_contracts/text_bytes.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("result <= 255", "result <= 254"),
        program.replace("ensures result == 233", "ensures result == 155"),
        program.replace("requires left == right\n", ""),
        program.replace(
            "index >= 0 && index < length(value)",
            "index < length(value)",
        ),
        program.replace("index >= 0 && index < length(value)", "index >= 0"),
        program.replace("index < length(value)", "index <= length(value)"),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsound Text byte proof: {changed}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    let local = r#"import std.text.byte
import std.text.length

fn observed(value Text, index Int) Int
ensures result >= 0 && result <= 255
{
    byte(value, index)
}

fn bounded(value Text, index Int) Bool
requires index >= 0 && index < length(value)
ensures result
{
    byte(value, index) >= 0 && byte(value, index) <= 255
}
"#;
    fs::write(&source, local).unwrap();
    success(
        &common::command(&["check", package.path().to_str().unwrap()])
            .env("PATH", "")
            .output()
            .unwrap(),
    );
    fs::write(
        &source,
        format!("{program}\nfn main() {{\n    text_byte_cases()\n}}\n"),
    )
    .unwrap();
    for mode in ["test", "run"] {
        success(&common::loom(&[mode, package.path().to_str().unwrap()]));
    }
}

#[test]
fn integer_division_intervals_prove_without_a_solver_but_keep_fault_checks() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("ranges.loom");
    let program = include_str!("../examples/smt_contracts/division_ranges.loom");
    let check = || {
        common::command(&["check", package.path().to_str().unwrap()])
            .env("PATH", "")
            .output()
            .unwrap()
    };
    fs::write(&source, program).unwrap();
    success(&check());
    for changed in [
        program.replace("result <= 9", "result <= 8"),
        program.replace("requires value >= 0\n", ""),
        program.replace("requires value != -9223372036854775808\n", ""),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound interval: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    fs::write(
        &source,
        format!("{program}\nfn main() {{\n    division_range_cases()\n}}\n"),
    )
    .unwrap();
    for mode in ["test", "run"] {
        success(&common::loom(&[mode, package.path().to_str().unwrap()]));
    }
}

#[test]
fn boolean_scans_recheck_duality_witnesses_and_guarded_suffix_safety() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("searches.loom");
    let program = include_str!("../examples/smt_contracts/searches.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("== !absent(values, needle)", "== absent(values, needle)"),
        program.replace("    fallback\n}", "    !fallback\n}"),
        program.replace(
            "        active = true\n        notes = previous.notes",
            "        active = false\n        notes = previous.notes",
        ),
        program.replace("requires needle != \"\"\n", ""),
        program.replace("index = index + 1", "index = index + 2"),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound scan proof: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved") || error.contains("scan"),
            "unexpected failure: {error}"
        );
    }
    let suffix = r#"import std.list.length

fn or_fault(values List[Int]) Bool {
    var index = 0
    while index < length(values) {
        if values[index] == 0 {
            return true
        }
        index = index + 1
    }
    values[length(values)] == 0
}

fn found(values List[Int]) Bool
requires length(values) > 0 && values[0] == 0
ensures result == or_fault(values)
{
    true
}
"#;
    fs::write(&source, suffix).unwrap();
    success(&check()); // A proved witness makes the faulty suffix unreachable.
    for changed in [
        suffix.replace("values[0] == 0\n", "values[0] == 1\n"),
        suffix.replace("if values[index] == 0", "if values[index + 1] == 0"),
    ] {
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsafe search abstraction: {changed}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    fs::write(&source, program).unwrap();
    success(&check());
}

#[test]
fn symbolic_division_rechecks_truncation_and_independent_fault_domains() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("division.loom");
    let program = include_str!("../examples/smt_contracts/division.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("    value / divisor\n}", "    value % divisor\n}"),
        program.replace("    value % divisor\n}", "    value / divisor\n}"),
        program.replace("ensures result == value / divisor", "ensures result >= 0"),
        program.replace(
            "value / 3 * 3 + value % 3 == value",
            "value / 3 * 3 + value % 3 == 0",
        ),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsound integer division: {changed}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    for unproved in [
        r#"fn unsafe(value Int, divisor Int) Bool
ensures value / divisor == value / divisor
{ true }
"#,
        r#"fn unsafe(value Int, divisor Int) Bool
requires divisor != 0
ensures value % divisor == value % divisor
{ true }
"#,
        r#"fn unsafe(value Int, divisor Int) Bool
ensures 0 * (value / divisor) == 0
{ true }
"#,
        r#"fn wrong_floor(value Int) Int
requires value == -7
ensures result == -3
{ value / 3 }
"#,
    ] {
        fs::write(&source, unproved).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsound division domain: {unproved}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "{error}"
        );
    }
    fs::write(&source, program).unwrap();
    success(&check());
    let artifact = common::executable(package.path(), "division");
    for (operation, value, divisor, message) in [
        ("integer_quotient", 1_i64, 0_i64, "division by zero"),
        ("integer_remainder", i64::MIN, -1, "integer overflow"),
    ] {
        fs::write(
            &source,
            format!("{program}\nfn main() {{\n    discard {operation}({value}, {divisor})\n}}\n"),
        )
        .unwrap();
        success(&common::loom(&[
            "build",
            package.path().to_str().unwrap(),
            "--output",
            artifact.to_str().unwrap(),
        ]));
        let output = Command::new(&artifact).output().unwrap();
        assert!(!output.status.success(), "division fault check disappeared");
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
    }
}

#[test]
fn typed_entry_snapshots_recheck_content_domains_and_post_state_indices() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("snapshots.loom");
    let program = include_str!("../examples/list_contracts/snapshots.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace(
            "let saved = values[observed]",
            "let saved = values[changed]",
        ),
        program.replace(
            "requires values[observed].ratio == values[observed].ratio\n",
            "",
        ),
        program.replace(
            "old(selected_entries(left, right, choose_left))",
            "old(selected_entries(left, right, !choose_left))",
        ),
        program.replace("old(values)[result].label", "old(values)[result + 1].label"),
        program.replace(
            "    if second {\n        1\n",
            "    if second {\n        2\n",
        ),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsound entry snapshot: {changed}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "unexpected failure: {error}"
        );
    }
    let opaque = package.path().join("opaque.loom");
    fs::write(
        &opaque,
        r#"import std.list.length

fn shared_child(values List[List[Int]]) Int
requires length(values) > 0 && length(values[0]) > 0
ensures result == old(values)[0][0]
{
    values[0][0]
}
"#,
    )
    .unwrap();
    fs::write(&source, program).unwrap();
    let output = check();
    assert!(
        !output.status.success(),
        "shared child became an immutable snapshot"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("shared contract observation requires a tracked List"),
        "unexpected failure: {error}"
    );
    fs::remove_file(opaque).unwrap();
    success(&check());
}

#[test]
fn typed_columns_recheck_invalid_contents_nan_and_aliases_after_cached_edits() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("scalars.loom");
    let program = include_str!("../examples/list_contracts/scalars.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace(
            "    values[index] = value\n}",
            "    values[index] = Content {\n        flags = (false, false)\n        label = value.label\n        ratio = value.ratio\n        notes = value.notes\n    }\n    values[index] = value\n}",
        ),
        program.replace("std.text.length(self.label) > 0 && ", ""),
        program.replace(
            "    push(values, value)\n}",
            "    push(values, Content {\n        flags = value.flags\n        label = value.label\n        ratio = 0.0 / 0.0\n        notes = value.notes\n    })\n}",
        ),
        program.replace("    values[changed] = saved", "    discard saved"),
        program.replace(
            "requires values[observed].ratio == values[observed].ratio\n",
            "",
        ),
        program
            .replace(
                "observed Int, changed Int)",
                "observed Int, changed Int, alias List[ScalarContent])",
            )
            .replace(
                "    values[changed] = saved",
                "    values[changed] = saved\n    alias[observed] = saved",
            )
            .replace("restore_content(saved, 0, 0)", "restore_content(saved, 0, 0, saved)"),
    ] {
        assert_ne!(changed, program, "test edit did not apply");
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound typed-column proof: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "unexpected failure: {error}"
        );
    }
    fs::write(&source, program).unwrap();
    success(&check());
}

#[test]
fn record_projections_compose_and_cached_edits_recheck_every_store() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let program = include_str!("../examples/list_contracts/projections.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace(
            "    values[index] = value\n}",
            "    values[index] = Row {\n        position = Position { amount = -1 }\n        bounds = (0, 10)\n        notes = []\n    }\n    values[index] = value\n}",
        ),
        program.replace(
            "    push(values, value)\n}",
            "    push(values, Row {\n        position = Position { amount = 11 }\n        bounds = (0, 10)\n        notes = []\n    })\n}",
        ),
        program.replace("    values[changed] = saved", "    discard saved"),
        program.replace(
            "    values[changed] = saved",
            "    values[changed] = saved\n    values[observed] = Frame {\n        position = Position { amount = 99 }\n        bounds = (99, 100)\n    }",
        ),
        program.replace("    length(values) > 0\n}", "    index == 0\n}"),
        program.replace("let row = values[index]", "let row = values[index + 1]"),
    ] {
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound record proof: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved") || error.contains("scan"),
            "unexpected failure: {error}"
        );
    }
    fs::write(&source, program).unwrap();
    success(&check());
}

#[test]
fn copy_contracts_compose_and_cached_edits_cannot_retain_stale_heap_facts() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("quantified.loom"),
        include_str!("../examples/smt_contracts/quantified.loom"),
    )
    .unwrap();
    let source = package.path().join("copies.loom");
    let program = include_str!("../examples/smt_contracts/copies.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    fs::write(&source, program).unwrap();
    success(&check());
    success(&check());
    for changed in [
        program.replace("push(output, get(values, index))", "push(output, 0)"),
        program.replace(
            "push(output, get(values, index))",
            "let alias = values\n        set(alias, index, 0)\n        push(output, get(values, index))",
        ),
        program
            .replace("let output = new[Int]()", "let output = values\n    let count = length(values)")
            .replace("index < length(values)", "index < count"),
        program.replace("    sort(output)\n", "    discard sort(output)\n    [0]\n"),
        program.replace(
            "    output\n}",
            "    if length(values) > 0 {\n        set(values, 0, 99)\n    }\n    output\n}",
        ),
    ] {
        fs::write(&source, &changed).unwrap();
        let output = check();
        assert!(!output.status.success(), "unsound copy contract: {changed}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved"),
            "unexpected failure: {error}"
        );
    }
    fs::write(&source, program).unwrap();
    success(&check());
    let construction = package.path().join("construction.loom");
    let wrapper = r#"type Sorted = List[Int] where ordered(self)

fn constrained(values List[Int]) Sorted {
    Sorted(sorted_copy(values))
}
"#;
    fs::write(&construction, wrapper).unwrap();
    success(&check());
    fs::write(
        &construction,
        wrapper.replace("sorted_copy(values)", "sort(values)"),
    )
    .unwrap();
    let aliased = check();
    assert!(!aliased.status.success());
    assert!(String::from_utf8_lossy(&aliased.stderr).contains("non-publishing factory"));
    fs::write(&construction, wrapper).unwrap();
    success(&check());
}

#[test]
fn quantified_contracts_reject_wrong_algorithms_and_unexecuted_safety_facts() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let example = include_str!("../examples/smt_contracts/quantified.loom");
    let cache = package.path().join("cache");
    let check = || {
        common::loom(&[
            "check",
            package.path().to_str().unwrap(),
            "--frontend-cache",
            cache.to_str().unwrap(),
        ])
    };
    for program in [
        example.to_owned(),
        example.replace(
            "        if get(values, index) == needle {",
            "        let candidate = get(values, index)\n        if candidate == needle {",
        ),
        example
            .replace(
                "fn sort(values",
                "record Batch {\n    values List[Int]\n}\n\nfn sort(values",
            )
            .replace(
                "    var end = length(values)",
                "    let batch = Batch { values = values }\n    var end = length(values)",
            )
            .replace(
                "    values\n}\n\nfn unchanged_order",
                "    return batch.values\n}\n\nfn unchanged_order",
            ),
    ] {
        fs::write(&source, program).unwrap();
        success(&check());
    }
    for program in [
        example.replace("if left > right", "if left < right"),
        example.replace("set(values, index, left)", "set(values, index, right)"),
        example.replace("set(values, second, before)", "set(values, second, after)"),
        example.replace(
            "get(values, index - 1) > get(values, index)",
            "get(values, index - 1) > get(values, index + 1)",
        ),
        example.replace("count = count + 1", "count = count + 2"),
        example.replace(
            "    var end = length(values)",
            "    if length(values) > 1 {\n        return values\n    }\n    var end = length(values)",
        ),
        example
            .replace("    var end = length(values)", "    var selected = values\n    var end = length(values)")
            .replace("    values\n}\n\nfn unchanged_order", "    selected = [9]\n    selected\n}\n\nfn unchanged_order"),
        example.replace(
            "    var end = length(values)",
            "    defer {\n        if length(values) > 1 {\n            set(values, 0, 99)\n        }\n    }\n    var end = length(values)",
        ),
        example.replace(
            "    values\n}",
            "    if length(values) > 1 {\n        set(values, 0, 99)\n    }\n    values\n}",
        ),
        r#"import std.list.length
import std.list.get

fn early_false(values List[Int]) Bool {
    var index = 0
    while index < length(values) {
        if get(values, index + 1) == 0 {
            return false
        }
        index = index + 1
    }
    true
}

fn impossible(values List[Int]) Bool
requires !early_false(values)
ensures result
{
    false
}
"#
        .to_owned(),
        r#"import std.list.length

fn dependent_count(values List[Int]) Int {
    var index = 0
    var count = 0
    while index < length(values) {
        let wanted = count
        if values[index] == wanted {
            count = count + 1
        }
        index = index + 1
    }
    count
}

fn wrong(values List[Int]) Int
requires length(values) == 2 && values[0] == 0 && values[1] == 1
ensures result == dependent_count(values)
{
    1
}
"#
        .to_owned(),
    ] {
        fs::write(&source, &program).unwrap();
        let output = check();
        assert!(
            !output.status.success(),
            "unsound quantified proof: {program}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("proof") || error.contains("proved") || error.contains("scan"),
            "unexpected failure: {error}"
        );
    }
}

#[test]
fn heap_versions_reject_wrong_writes_alias_interference_and_undefined_reads() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    for program in [
        RESTORE.replace("set(values, changed, saved)", "set(values, changed, 7)"),
        RESTORE
            .replace("changed Int)", "changed Int, other List[Int])")
            .replace(
                "ensures get",
                "requires changed < length(other)\nensures get",
            )
            .replace(
                "set(values, changed, saved)",
                "set(values, changed, saved)\n    set(other, changed, 9)",
            ),
        format!(
            "{}\nfn change(values List[Int], index Int) {{\n    set(values, index, 9)\n}}\n",
            RESTORE.replace(
                "set(values, changed, saved)",
                "set(values, changed, saved)\n    change(values, changed)",
            )
        ),
        RESTORE.replace("requires observed >= 0 && observed < length(values)\n", ""),
    ] {
        fs::write(&source, &program).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound heap proof: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("postcondition") || error.contains("List element proof"),
            "unexpected failure: {error}\n{program}"
        );
    }
}

#[test]
fn cached_heap_proofs_revalidate_edits_and_worker_interference() {
    for program in [
        RESTORE.to_owned(),
        RESTORE.replace("old(get(values, observed))", "get(old(values), observed)"),
    ] {
        let package = tempfile::tempdir().unwrap();
        let cache = package.path().join("cache");
        let source = package.path().join("restore.loom");
        let main = package.path().join("main.loom");
        fs::write(&source, &program).unwrap();
        fs::write(&main, "fn main() {\n    restored([1], 0, 0)\n}\n").unwrap();
        let check = || {
            common::loom(&[
                "check",
                package.path().to_str().unwrap(),
                "--frontend-cache",
                cache.to_str().unwrap(),
            ])
        };
        success(&check());
        success(&check());
        fs::write(
            &source,
            program.replace("set(values, changed, saved)", "set(values, changed, 7)"),
        )
        .unwrap();
        let wrong = check();
        assert!(!wrong.status.success());
        assert!(String::from_utf8_lossy(&wrong.stderr).contains("postcondition"));
        fs::write(&source, &program).unwrap();
        success(&check());
        fs::write(
            &main,
            r#"import std.task.worker.run

async fn main() {
    discard run(fn() Int {
            restored([1], 0, 0)
            1
        }).await
}
"#,
        )
        .unwrap();
        let shared = check();
        assert!(!shared.status.success());
        let error = String::from_utf8_lossy(&shared.stderr);
        assert!(error.contains("interference-safe"), "{error}");
    }
}

#[test]
fn entry_lists_do_not_restore_current_headers_or_hide_undefined_accesses() {
    let package = tempfile::tempdir().unwrap();
    let source = package.path().join("main.loom");
    let snapshot = RESTORE.replace("old(get(values, observed))", "get(old(values), observed)");
    for program in [
        snapshot.replace("set(values, changed, saved)", "set(values, changed, 7)"),
        snapshot.replace(
            "get(old(values), observed)",
            "get(old(values), length(values))",
        ),
        r#"import std.list.get
import std.list.set
import std.list.length

fn changed(values List[Int], index Int) Int
requires index >= 0 && index < length(values)
ensures result == get(old(values), index)
{
    let saved = get(values, index)
    set(values, index, 7)
    saved
}

fn wrong(values List[Int], index Int) Int
requires index >= 0 && index < length(values)
ensures result == get(values, index)
{
    changed(values, index)
}
"#
        .to_owned(),
        r#"import std.list.get
import std.list.length

fn wrong(values List[Int], index Int) Bool
requires index >= 0 && index < length(values)
ensures get(old(values), index) + 1 > get(old(values), index)
{
    true
}
"#
        .to_owned(),
    ] {
        fs::write(&source, &program).unwrap();
        let output = common::loom(&["check", package.path().to_str().unwrap()]);
        assert!(
            !output.status.success(),
            "unsound entry snapshot: {program}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("postcondition") || error.contains("List element proof"),
            "unexpected failure: {error}\n{program}"
        );
    }
}

#[test]
fn solver_contracts_keep_editor_concept_navigation() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    let text = r#"concept Read {
    fn read(self Self) Int
}

fn read[T Read](value T) Int {
    value.read()
}

fn square(value Int) Int
ensures result >= 0
{
    value * value
}
"#;
    fs::write(&source, text).unwrap();
    let output = common::loom(&[
        "editor-query",
        temporary.path().to_str().unwrap(),
        "--at",
        source.to_str().unwrap(),
        &text.find("Read]").unwrap().to_string(),
    ]);
    success(&output);
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("\"diagnostics\":[]"), "{report}");
    assert!(report.contains("\"start\":8,\"end\":12"), "{report}");
    assert!(!report.contains("\"definitions\":[]"), "{report}");
}

#[test]
fn solver_contracts_check_and_compile_without_a_runtime_solver() {
    let package = "compiler/examples/smt_contracts";
    for command in ["check", "test", "run"] {
        success(&common::loom(&[command, package]));
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = common::executable(temporary.path(), "smt-contracts");
    let ir = temporary.path().join("app.ll");
    for level in ["0", "2"] {
        success(
            &common::command(&["build", package])
                .arg("--output")
                .arg(&executable)
                .arg("--emit-ir")
                .arg(&ir)
                .env("LOOM_OPT_LEVEL", level)
                .output()
                .unwrap(),
        );
        let emitted = fs::read_to_string(&ir).unwrap();
        assert!(!emitted.contains("process_capture"));
        assert!(!emitted.contains("loom_bag") && !emitted.contains("proof_all"));
        success(
            &Command::new(&executable)
                .env("PATH", "")
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn solver_refutes_counterexamples_and_does_not_rescue_undefined_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    for program in [
        r#"import std.text.length
fn wrong(value Text) Bool
requires value == "雪" || value == "é"
ensures result
{ length(value) > 2 }
"#,
        r#"fn wrong(value Int) Int
requires value >= -100 && value <= 100
ensures result != 2
{ value * 2 }
"#,
        r#"fn wrong(value Int) Int
ensures value + 1 > value
{ 0 }
"#,
        r#"import std.text.length
fn wrong(value Text, number Int) Bool
requires length(value) == 0
ensures value != "" || number + 1 > number
{ true }
"#,
        r#"import std.text.length
fn either(first Bool, second Bool) Bool { first || second }
fn wrong(value Text, number Int) Bool
requires length(value) == 0
ensures either(value == "", number + 1 > number)
{ true }
"#,
        r#"fn wrong(value Int) Int
ensures result > 0
{ value * value }
"#,
        r#"fn wrong(value Int) Bool
ensures value * value >= 0
{ true }
"#,
        r#"fn wrong(value Int) Int
requires value >= -100 && value <= 100
ensures result == value * value + 1
{ value * value }
"#,
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound proof: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("postcondition"),
            "unexpected failure: {error}"
        );
    }
    fs::write(
        &source,
        r#"fn identity(value Int) Int ensures result == value { value }
fn bounded(value Int) Int
requires value >= -3 && value <= 4
ensures result <= 16
{
    value * value
}
"#,
    )
    .unwrap();
    success(
        &common::command(&["check", temporary.path().to_str().unwrap()])
            .env("PATH", "")
            .output()
            .unwrap(),
    );
    let output = common::command(&["check", "compiler/examples/smt_contracts"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("required proof needs Z3"));
}

#[test]
fn solver_refinement_proofs_erase_checks_but_not_eager_faults() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("main.loom");
    fs::write(
        &source,
        r#"import std.list.get
import std.list.set

type Small = Int where self >= -10 && self <= 10

fn square_nonnegative(value Int) Bool {
    value * value >= 0
}

type SafeSquare = Int where square_nonnegative(self)

fn observed(counter List[Int]) Small {
    set(counter, 0, get(counter, 0) + 1)
    Small(3)
}

fn weaken(value Small) SafeSquare {
    SafeSquare(value)
}

fn main() {
    let counter = [0]
    assert weaken(observed(counter)) == 3
    assert get(counter, 0) == 1
}
"#,
    )
    .unwrap();
    let executable = common::executable(temporary.path(), "refinements");
    let ir = temporary.path().join("refinements.ll");
    success(
        &common::command(&["build", temporary.path().to_str().unwrap()])
            .arg("--output")
            .arg(&executable)
            .arg("--emit-ir")
            .arg(&ir)
            .env("LOOM_OPT_LEVEL", "0")
            .output()
            .unwrap(),
    );
    assert!(
        !fs::read_to_string(&ir).unwrap().contains("llvm.smul"),
        "proved predicate must disappear before LLVM optimization"
    );
    success(&Command::new(&executable).output().unwrap());
    for program in [
        r#"type UnitSquare = Int where self * self == 1
type Positive = Int where self > 0
fn wrong(value UnitSquare) Positive {
    Positive(value)
}
"#,
        r#"fn square_nonnegative(value Int) Bool {
    value * value >= 0
}
type SafeSquare = Int where square_nonnegative(self)
fn wrong(value Int) SafeSquare {
    SafeSquare(value)
}
"#,
        r#"fn either(first Bool, second Bool) Bool {
    first || second
}
type SafeNext = Int where either(true, self + 1 > self)
fn wrong(value Int) SafeNext {
    SafeNext(value)
}
"#,
    ] {
        fs::write(&source, program).unwrap();
        let output = common::loom(&["check", temporary.path().to_str().unwrap()]);
        assert!(!output.status.success(), "unsound refinement: {program}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Result["), "unexpected failure: {error}");
    }
}
