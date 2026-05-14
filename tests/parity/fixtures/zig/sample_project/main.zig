const std = @import("std");
const utils = @import("utils.zig");

pub fn main() void {
    const result = utils.add(1, 2);
    std.debug.print("Result: {}\n", .{result});
    utils.greet("world");
}

pub fn runTests() void {
    _ = utils.add(10, 20);
}
