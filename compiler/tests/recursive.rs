use std::fs;
mod common;

fn checked(source: &str) -> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.loom"), source).unwrap();
    let output = common::loom(&["check", directory.path().to_str().unwrap()]);
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8(output.stderr).unwrap())
    }
}

#[test]
fn recursive_nominal_types_need_indirect_layout_edges() {
    for source in [
        "enum Expr { Number(Int) Call(List[Expr]) }",
        "record Tree[T] { value T children List[Tree[T]] }",
        "record A[T] { children List[B[T]] } enum B[T] { End(T) More(A[T]) }",
        "record Swap[A, B] { children List[Swap[B, A]] } fn use(x Swap[Int, Bool]) {}",
    ] {
        checked(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
    for source in [
        "record Cycle { next Cycle }",
        "record A { next B } enum B { End More(A) }",
        "record Outer { values List[Inner] } record Inner { next Inner }",
        "record Wrap[T] { value T } record Cycle { next Wrap[Cycle] }",
    ] {
        let error = checked(source).unwrap_err();
        assert!(error.contains("recursive by-value"), "{source}: {error}");
    }
    for source in [
        "record Grow[T] { children List[Grow[List[T]]] }",
        "record Grow[T] { children List[Grow[Grow[T]]] }",
    ] {
        let error = checked(source).unwrap_err();
        assert!(error.contains("specialization"), "{source}: {error}");
    }
}

#[test]
fn native_recursive_data_survives_collection_and_shared_cycles() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.list.new
import std.list.length
import std.list.get
import std.list.push
import std.text.concat

enum Expr { Number(Int, Text) Sum(List[Expr]) }
record Tree[T] { value T children List[Tree[T]] }

fn sum(value Expr) Int {
    match value {
        Expr.Number(number, _) => number
        Expr.Sum(children) => {
            var total = 0
            var index = 0
            while index < length(children) {
                total = total + sum(get(children, index))
                index = index + 1
            }
            total
        }
    }
}

fn identity[T](value Tree[T]) Tree[T] { value }

fn exercise() {
    let terms = new[Expr]()
    let root = Expr.Sum(terms)
    var index = 0
    while index < 40 {
        push(terms, Expr.Number(index, concat("managed", " value")))
        index = index + 1
    }
    assert sum(root) == 780
    let children = new[Tree[Int]]()
    let tree = identity(Tree { value = 7 children = children })
    push(children, Tree { value = 9 children = new[Tree[Int]]() })
    assert get(tree.children, 0).value == 9

    // Cycles are ordinary shared data, and tracing must stop after marking them.
    push(terms, root)
    index = 0
    while index < 40 {
        let garbage = new[Text]()
        push(garbage, concat("temporary", " text"))
        index = index + 1
    }
    assert length(terms) == 41
    assert get(tree.children, 0).value == 9
}

fn main() { exercise() }
test fn recursive_data() { exercise() }
"#,
    )
    .unwrap();
    for operation in ["check", "run", "test"] {
        let args = [operation, directory.path().to_str().unwrap()];
        let output = if operation == "check" {
            common::loom(&args)
        } else {
            common::managed(&args, directory.path())
        };
        assert!(
            output.status.success(),
            "{operation}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
