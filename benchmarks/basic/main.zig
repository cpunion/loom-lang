const std = @import("std");

fn advance(value: i64) i64 {
    return @mod(value * 17 + 23, 1_000_003);
}

fn alternate(value: i64) i64 {
    return @mod(value * 7 + 11, 1_000_003);
}

fn intLcg(n: i64, seed: i64) i64 {
    var state = @mod(seed, 1_000_003);
    var i: i64 = 0;
    while (i < n) : (i += 1) state = advance(state);
    return state;
}

fn fibRecursive(n: i64) i64 {
    return if (n < 2) n else fibRecursive(n - 1) + fibRecursive(n - 2);
}

const Point = struct { x: i64, y: i64, z: i64 };

fn step(point: Point, delta: i64) Point {
    return .{
        .x = @mod(point.y + delta, 1_000_003),
        .y = @mod(point.z + point.x, 1_000_003),
        .z = @mod(point.x + point.y + point.z, 1_000_003),
    };
}

fn recordValue(n: i64, seed: i64) i64 {
    var point = Point{ .x = @mod(seed, 97), .y = @mod(seed, 193), .z = @mod(seed, 389) };
    var i: i64 = 0;
    while (i < n) : (i += 1) point = step(point, @mod(i, 31));
    return point.x + point.y + point.z;
}

fn listBuildScan(allocator: std.mem.Allocator, n: i64, seed: i64) !i64 {
    var values: std.ArrayList(i64) = .empty;
    defer values.deinit(allocator);
    var state = @mod(seed, 1_000_003);
    var i: i64 = 0;
    while (i < n) : (i += 1) {
        state = advance(state);
        try values.append(allocator, state);
    }
    var sum: i64 = 0;
    for (values.items) |value| sum += value;
    return sum;
}

fn functionValue(n: i64, seed: i64) i64 {
    const action: *const fn (i64) i64 = if (@mod(seed, 2) == 0) &advance else &alternate;
    var state = @mod(seed, 1_000_003);
    var i: i64 = 0;
    while (i < n) : (i += 1) state = action(state);
    return state;
}

pub fn main(init: std.process.Init) !void {
    var args = try std.process.Args.Iterator.initAllocator(init.minimal.args, init.gpa);
    defer args.deinit();
    _ = args.skip();
    const case = args.next() orelse return error.ExpectedCase;
    const n = try std.fmt.parseInt(i64, args.next() orelse return error.ExpectedN, 10);
    const seed = try std.fmt.parseInt(i64, args.next() orelse return error.ExpectedSeed, 10);
    if (n < 0 or seed < 0 or args.next() != null) return error.InvalidArguments;

    const checksum = if (std.mem.eql(u8, case, "int_lcg")) intLcg(n, seed) else if (std.mem.eql(u8, case, "fib_recursive")) fibRecursive(n) else if (std.mem.eql(u8, case, "record_value")) recordValue(n, seed) else if (std.mem.eql(u8, case, "list_build_scan")) try listBuildScan(init.gpa, n, seed) else if (std.mem.eql(u8, case, "function_value")) functionValue(n, seed) else return error.UnknownCase;
    var buffer: [64]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(init.io, &buffer);
    try stdout.interface.print("{d}\n", .{checksum});
    try stdout.interface.flush();
}
