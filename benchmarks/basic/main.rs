const MODULUS: i64 = 1_000_003;

fn advance(value: i64) -> i64 {
    (value * 17 + 23) % MODULUS
}

fn alternate(value: i64) -> i64 {
    (value * 7 + 11) % MODULUS
}

fn int_lcg(n: i64, seed: i64) -> i64 {
    let mut state = seed % MODULUS;
    for _ in 0..n {
        state = advance(state);
    }
    state
}

fn fib_recursive(n: i64) -> i64 {
    if n < 2 {
        n
    } else {
        fib_recursive(n - 1) + fib_recursive(n - 2)
    }
}

struct Point {
    x: i64,
    y: i64,
    z: i64,
}

fn step(old: Point, delta: i64) -> Point {
    Point {
        x: (old.y + delta) % MODULUS,
        y: (old.z + old.x) % MODULUS,
        z: (old.x + old.y + old.z) % MODULUS,
    }
}

fn record_value(n: i64, seed: i64) -> i64 {
    let mut point = Point {
        x: seed % 97,
        y: seed % 193,
        z: seed % 389,
    };
    for i in 0..n {
        point = step(point, i % 31);
    }
    point.x + point.y + point.z
}

fn list_build_scan(n: i64, seed: i64) -> i64 {
    let mut values = Vec::new();
    let mut state = seed % MODULUS;
    for _ in 0..n {
        state = advance(state);
        values.push(state);
    }
    let mut total: i64 = 0;
    for value in values {
        total += value;
    }
    total
}

fn function_value(n: i64, seed: i64) -> i64 {
    let action: fn(i64) -> i64 = if seed % 2 == 0 { advance } else { alternate };
    let mut state = seed % MODULUS;
    for _ in 0..n {
        state = action(state);
    }
    state
}

fn main() {
    let arguments: Vec<_> = std::env::args().collect();
    assert_eq!(arguments.len(), 4, "usage: benchmark CASE N SEED");
    let n = arguments[2].parse::<i64>().expect("integer N");
    let seed = arguments[3].parse::<i64>().expect("integer SEED");
    let checksum = match arguments[1].as_str() {
        "int_lcg" => int_lcg(n, seed),
        "fib_recursive" => fib_recursive(n),
        "record_value" => record_value(n, seed),
        "list_build_scan" => list_build_scan(n, seed),
        "function_value" => function_value(n, seed),
        _ => panic!("unknown benchmark case"),
    };
    println!("{checksum}");
}
