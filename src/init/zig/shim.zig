const std = @import("std");
const solution = @import("solution");

extern fn write(ptr: [*]const u8, len: usize) void;

export fn input(len: usize) [*]u8 {
    return (std.heap.wasm_allocator.alloc(u8, len) catch @trap()).ptr;
}

export fn run(ptr: [*]const u8, len: usize) void {
    var in: std.Io.Reader = .fixed(ptr[0..len]);
    var out: std.Io.Writer.Allocating = .init(std.heap.wasm_allocator);
    solution.solve(&in, &out.writer) catch @trap();
    write(out.written().ptr, out.written().len);
}
