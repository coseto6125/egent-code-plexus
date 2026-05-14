const std = @import("std");

pub const MAX_VALUE: u32 = 100;

pub const Point = struct {
    x: f32,
    y: f32,

    pub fn init(x: f32, y: f32) Point {
        return Point{ .x = x, .y = y };
    }

    pub fn distance(self: Point, other: Point) f32 {
        const dx = self.x - other.x;
        const dy = self.y - other.y;
        return std.math.sqrt(dx * dx + dy * dy);
    }
};

pub fn add(a: u32, b: u32) u32 {
    return a + b;
}

pub fn greet(name: []const u8) void {
    std.debug.print("Hello, {s}!\n", .{name});
}
