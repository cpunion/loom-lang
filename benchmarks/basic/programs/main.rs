use std::env;
use std::process::ExitCode;

const MODULUS: i64 = 2_147_483_647;
const MULTIPLIER: i64 = 48_271;

#[derive(Clone, Copy)]
struct Counter {
    total: i64,
    calls: i64,
}

impl Counter {
    fn add(&mut self, value: i64) {
        self.total += value;
        self.calls += 1;
    }
}

fn modular_product(left: i64, right: i64) -> i64 {
    let product = left * right;
    product - (product / MODULUS) * MODULUS
}

fn int_lcg(size: i64) -> i64 {
    let mut state = 1_i64;
    for _ in 0..size {
        state = modular_product(state, MULTIPLIER);
    }
    state
}

fn periodic_value(index: i64) -> i64 {
    index - (index / 1024) * 1024
}

fn record_method(size: i64) -> Counter {
    let mut value = Counter { total: 0, calls: 0 };
    for index in 0..size {
        value.add(periodic_value(index));
    }
    value
}

fn list_build_scan(size: i64) -> Result<i64, &'static str> {
    let mut values = Vec::new();
    for index in 0..size {
        values.push(periodic_value(index));
    }
    let mut checksum = 0_i64;
    for index in 0..size {
        let index = usize::try_from(index).map_err(|_| "list index outside bounds")?;
        checksum += *values.get(index).ok_or("list index outside bounds")?;
    }
    if i64::try_from(values.len()).map_err(|_| "list length outside i64")? != size {
        return Err("list length mismatch");
    }
    Ok(checksum)
}

fn fibonacci(value: i64) -> i64 {
    if value < 2 {
        value
    } else {
        fibonacci(value - 1) + fibonacci(value - 2)
    }
}

fn run(name: &str, size: i64, expected: i64) -> Result<(), &'static str> {
    if size < 0 {
        return Err("size must be non-negative");
    }
    match name {
        "int_lcg" => {
            if size > 100_000_000 {
                return Err("int_lcg size exceeds limit");
            }
            if int_lcg(size) != expected {
                return Err("int_lcg checksum mismatch");
            }
        }
        "record_method" => {
            if size > 100_000_000 {
                return Err("record_method size exceeds limit");
            }
            let value = record_method(size);
            if value.total != expected || value.calls != size {
                return Err("record_method checksum mismatch");
            }
        }
        "list_build_scan" => {
            if size > 10_000_000 {
                return Err("list_build_scan size exceeds limit");
            }
            if list_build_scan(size)? != expected {
                return Err("list_build_scan checksum mismatch");
            }
        }
        "fib_recursive" => {
            if size > 45 {
                return Err("fib_recursive size exceeds limit");
            }
            if fibonacci(size) != expected {
                return Err("fib_recursive checksum mismatch");
            }
        }
        _ => return Err("unknown benchmark case"),
    }
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() != 3 {
        eprintln!("usage: benchmark CASE SIZE EXPECTED");
        return ExitCode::from(2);
    }
    let Ok(size) = arguments[1].parse::<i64>() else {
        eprintln!("invalid benchmark size");
        return ExitCode::from(2);
    };
    let Ok(expected) = arguments[2].parse::<i64>() else {
        eprintln!("invalid expected checksum");
        return ExitCode::from(2);
    };
    if let Err(error) = run(&arguments[0], size, expected) {
        eprintln!("{error}");
        return ExitCode::from(2);
    }
    print!("Unit\n");
    ExitCode::SUCCESS
}
