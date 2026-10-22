const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;
const codeforces = @import("codeforces.zig");
const statement = @import("statement.zig");
const Watch = @import("Watch.zig");

pub const Lang = struct {
    ext: []const u8,
    comment: []const u8,
    build: []const u8,
    run: []const u8,
    submit: []const u8,
    judge: []const u8,
};

pub const Config = struct {
    handle: []const u8,
    lang: []const u8,
    langs: std.json.ArrayHashMap(Lang),
};

const State = struct {
    last: ?[]const u8 = null,
    fresh: std.json.ArrayHashMap(i64) = .{},
};

const assets = .{
    .{ "cf.json", @embedFile("init/cf.json") },
    .{ ".gitignore", @embedFile("init/gitignore") },
    .{ "zig/shim.zig", @embedFile("init/zig/shim.zig") },
    .{ "zig/run.js", @embedFile("init/zig/run.js") },
    .{ "templates/main.zig", @embedFile("init/templates/main.zig") },
    .{ "templates/main.cpp", @embedFile("init/templates/main.cpp") },
};

pub fn main(init: std.process.Init) !void {
    const gpa = init.arena.allocator();
    const io = init.io;
    const args = try init.minimal.args.toSlice(gpa);
    var cf: codeforces.Client = try .init(gpa, io, init.environ_map);
    const command = if (args.len > 1) args[1] else "";

    if (std.mem.eql(u8, command, "init")) {
        const cwd = Io.Dir.cwd();
        inline for (assets) |asset| {
            if (std.fs.path.dirname(asset[0])) |dir| try cwd.createDirPath(io, dir);
            const data = if (std.mem.eql(u8, asset[0], "cf.json"))
                try std.mem.replaceOwned(u8, gpa, asset[1], "HANDLE", args[2])
            else
                asset[1];
            try cwd.writeFile(io, .{ .sub_path = asset[0], .data = data });
        }
        try cwd.createDirPath(io, "lists");
        try cwd.createDirPath(io, "problems");
        _ = try cf.problems(true);
        return;
    }

    const config = try std.json.parseFromSliceLeaky(Config, gpa, try Io.Dir.cwd().readFileAlloc(io, "cf.json", gpa, .unlimited), .{});

    if (std.mem.eql(u8, command, "sync")) {
        _ = try cf.problems(true);
        _ = try cf.submissions(config.handle);
        return;
    }

    var state = std.json.parseFromSliceLeaky(State, gpa, cf.cache.readFileAlloc(io, "state.json", gpa, .unlimited) catch "{}", .{}) catch State{};
    const root = try std.process.currentPathAlloc(io, gpa);
    const fresh = args.len > 1 and std.mem.eql(u8, args[args.len - 1], "--fresh");
    const sources = args[1 .. args.len - @intFromBool(fresh)];
    const path = if (sources.len > 0) try list(gpa, io, &cf, sources) else state.last orelse return error.NoList;
    const key = try std.fs.path.join(gpa, &.{ root, path });

    state.last = path;
    if (fresh) try state.fresh.map.put(gpa, key, Io.Timestamp.now(io, .real).toSeconds());
    try cf.cache.writeFile(io, .{ .sub_path = "state.json", .data = try std.json.Stringify.valueAlloc(gpa, state, .{}) });

    const text = try Io.Dir.cwd().readFileAlloc(io, path, gpa, .unlimited);
    var watch: Watch = .{
        .gpa = gpa,
        .io = io,
        .cf = &cf,
        .config = config,
        .lang = config.langs.map.get(config.lang).?,
        .path = path,
        .ids = try statement.ids(gpa, text),
        .since = state.fresh.map.get(key) orelse 0,
        .problems = try cf.problems(false),
        .submissions = try cf.submissions(config.handle),
    };
    try watch.loop();
}

fn list(gpa: Allocator, io: Io, cf: *codeforces.Client, sources: []const [:0]const u8) ![]const u8 {
    const source = sources[0];
    if (!std.mem.eql(u8, source, "-") and !std.mem.startsWith(u8, source, "http")) return source;

    var stdin = Io.File.stdin().reader(io, &.{});
    const text = if (std.mem.eql(u8, source, "-"))
        try stdin.interface.allocRemaining(gpa, .unlimited)
    else
        try cf.get(source);
    const name = if (sources.len > 1) sources[1] else std.fs.path.basename(std.mem.trimEnd(u8, source, "/"));
    const path = try std.fmt.allocPrint(gpa, "lists/{s}.txt", .{name});
    var out: Io.Writer.Allocating = .init(gpa);
    for (try statement.ids(gpa, text)) |id| try out.writer.print("{s}\n", .{id});
    try Io.Dir.cwd().writeFile(io, .{ .sub_path = path, .data = out.written() });
    return path;
}

test {
    _ = statement;
}
