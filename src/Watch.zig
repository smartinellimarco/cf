const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;
const main = @import("main.zig");
const codeforces = @import("codeforces.zig");
const statement = @import("statement.zig");

const Watch = @This();

gpa: Allocator,
io: Io,
cf: *codeforces.Client,
config: main.Config,
lang: main.Lang,
path: []const u8,
ids: []const []const u8,
since: i64,
problems: []codeforces.Problem,
submissions: []codeforces.Submission,

catalog: std.array_hash_map.String(codeforces.Problem) = .empty,
current: usize = 0,
cursor: ?usize = null,
signature: i96 = -1,
report: []const u8 = "",
passed: bool = false,
pending: ?i64 = null,
polled: Io.Timestamp = .zero,
verdict: []const u8 = "",

const green = "\x1b[32m";
const red = "\x1b[31m";
const dim = "\x1b[2m";
const bold = "\x1b[1m";
const reset = "\x1b[0m";

pub fn loop(w: *Watch) !void {
    for (w.problems) |problem| try w.catalog.put(w.gpa, try codeforces.id(w.gpa, problem), problem);
    w.current = w.next(0);
    try w.open();

    const tty = try Io.Dir.cwd().openFile(w.io, "/dev/tty", .{ .mode = .read_write });
    const original = try std.posix.tcgetattr(tty.handle);
    var raw = original;
    raw.lflag.ICANON = false;
    raw.lflag.ECHO = false;
    // Ctrl-C arrives as a key so the terminal is always restored.
    raw.lflag.ISIG = false;
    // read returns after 100 ms without a key, which doubles as the file watch tick.
    raw.cc[@intFromEnum(std.posix.V.MIN)] = 0;
    raw.cc[@intFromEnum(std.posix.V.TIME)] = 1;
    try std.posix.tcsetattr(tty.handle, .NOW, raw);
    defer std.posix.tcsetattr(tty.handle, .NOW, original) catch {};
    try w.write("\x1b[?1049h\x1b[?25l");
    defer w.write("\x1b[?25h\x1b[?1049l") catch {};

    var dirty = true;
    while (true) {
        if (dirty) try w.render();
        dirty = false;

        var key: [1]u8 = undefined;
        if (try std.posix.read(tty.handle, &key) > 0) {
            if (!try w.press(key[0])) return;
            dirty = true;
        }

        const signature = try w.stamp();
        if (signature != w.signature) {
            w.signature = signature;
            w.report = "building…";
            try w.render();
            try w.check();
            dirty = true;
        }

        if (w.pending != null and w.polled.durationTo(.now(w.io, .awake)).nanoseconds > Io.Duration.fromSeconds(5).nanoseconds) {
            w.polled = .now(w.io, .awake);
            try w.poll();
            dirty = true;
        }
    }
}

fn press(w: *Watch, key: u8) !bool {
    if (w.cursor) |cursor| {
        switch (key) {
            'j' => w.cursor = @min(cursor + 1, w.ids.len - 1),
            'k' => w.cursor = cursor -| 1,
            '\n', '\r' => {
                w.current = cursor;
                w.cursor = null;
                try w.open();
            },
            'l', 'q', 27 => w.cursor = null,
            3 => return false,
            else => {},
        }
        return true;
    }
    switch (key) {
        'q', 3 => return false,
        'y' => try w.copy(),
        'o' => try w.submit(),
        'l' => w.cursor = w.current,
        'n' => {
            w.current = w.next(w.current + 1);
            try w.open();
        },
        'r' => w.signature = -1,
        else => {},
    }
    return true;
}

fn status(w: *Watch, id: []const u8) enum { solved, tried, todo } {
    var tried = false;
    for (w.submissions) |sub| {
        if (sub.creationTimeSeconds < w.since or sub.problem.contestId == 0) continue;
        if (!w.same(sub.problem, id)) continue;
        if (std.mem.eql(u8, sub.verdict orelse "", "OK")) return .solved;
        tried = true;
    }
    return if (tried) .tried else .todo;
}

fn same(w: *Watch, problem: codeforces.Problem, id: []const u8) bool {
    _ = w;
    var buffer: [32]u8 = undefined;
    const other = std.fmt.bufPrint(&buffer, "{d}{s}", .{ problem.contestId, problem.index }) catch return false;
    return std.mem.eql(u8, other, id);
}

fn next(w: *Watch, from: usize) usize {
    for (0..w.ids.len) |i| {
        const at = (from + i) % w.ids.len;
        if (w.status(w.ids[at]) != .solved) return at;
    }
    return from % w.ids.len;
}

fn solved(w: *Watch) usize {
    var count: usize = 0;
    for (w.ids) |id| count += @intFromBool(w.status(id) == .solved);
    return count;
}

fn split(id: []const u8) struct { []const u8, []const u8 } {
    const i = std.mem.indexOfNone(u8, id, "0123456789").?;
    return .{ id[0..i], id[i..] };
}

fn dir(w: *Watch) ![]const u8 {
    return std.fmt.allocPrint(w.gpa, "problems/{s}", .{w.ids[w.current]});
}

fn url(w: *Watch, page: []const u8) ![]const u8 {
    const contest, const index = split(w.ids[w.current]);
    return std.fmt.allocPrint(w.gpa, "https://codeforces.com/contest/{s}/{s}{s}", .{ contest, page, index });
}

fn open(w: *Watch) !void {
    w.signature = -1;
    w.pending = null;
    w.verdict = "";
    const cwd = Io.Dir.cwd();
    const path = try std.fmt.allocPrint(w.gpa, "{s}/main.{s}", .{ try w.dir(), w.lang.ext });
    cwd.access(w.io, path, .{}) catch {
        const problem_url = try w.url("problem/");
        const page = try statement.parse(w.gpa, try w.cf.get(problem_url));
        try cwd.createDirPath(w.io, try w.dir());
        for (page.samples, 1..) |sample, k| {
            try cwd.writeFile(w.io, .{ .sub_path = try std.fmt.allocPrint(w.gpa, "{s}/in{d}.txt", .{ try w.dir(), k }), .data = sample.input });
            try cwd.writeFile(w.io, .{ .sub_path = try std.fmt.allocPrint(w.gpa, "{s}/ans{d}.txt", .{ try w.dir(), k }), .data = sample.answer });
        }
        const template = try cwd.readFileAlloc(w.io, try std.fmt.allocPrint(w.gpa, "templates/main.{s}", .{w.lang.ext}), w.gpa, .unlimited);
        const header = try statement.comment(w.gpa, w.lang.comment, page, problem_url);
        try cwd.writeFile(w.io, .{ .sub_path = path, .data = try std.mem.concat(w.gpa, u8, &.{ header, template }) });
    };
}

fn stamp(w: *Watch) !i96 {
    var problem = try Io.Dir.cwd().openDir(w.io, try w.dir(), .{ .iterate = true });
    defer problem.close(w.io);
    const source = try std.fmt.allocPrint(w.gpa, "main.{s}", .{w.lang.ext});
    var signature: i96 = 0;
    var entries = problem.iterate();
    while (try entries.next(w.io)) |entry| {
        const sample = std.mem.endsWith(u8, entry.name, ".txt") and (std.mem.startsWith(u8, entry.name, "in") or std.mem.startsWith(u8, entry.name, "ans"));
        if (!sample and !std.mem.eql(u8, entry.name, source)) continue;
        const stat = try problem.statFile(w.io, entry.name, .{});
        signature +%= stat.mtime.nanoseconds +% @as(i96, @intCast(entry.name.len));
    }
    return signature;
}

fn sh(w: *Watch, command: []const u8, seconds: i64) !std.process.RunResult {
    return std.process.run(w.gpa, w.io, .{
        .argv = &.{ "sh", "-c", command },
        .cwd = .{ .path = try w.dir() },
        .timeout = .{ .duration = .{ .raw = .fromSeconds(seconds), .clock = .awake } },
    });
}

fn check(w: *Watch) !void {
    w.passed = false;
    var out: Io.Writer.Allocating = .init(w.gpa);
    const r = &out.writer;
    defer w.report = out.written();

    const build = try w.sh(w.lang.build, 120);
    if (!build.term.success()) {
        try r.print("{s}build failed{s}\n{s}{s}", .{ red, reset, build.stdout, build.stderr });
        return;
    }

    var passed = true;
    var k: usize = 1;
    while (true) : (k += 1) {
        const input = try std.fmt.allocPrint(w.gpa, "{s}/in{d}.txt", .{ try w.dir(), k });
        const answer = Io.Dir.cwd().readFileAlloc(w.io, try std.fmt.allocPrint(w.gpa, "{s}/ans{d}.txt", .{ try w.dir(), k }), w.gpa, .unlimited) catch break;
        Io.Dir.cwd().access(w.io, input, .{}) catch break;

        const start: Io.Timestamp = .now(w.io, .awake);
        const run = w.sh(try std.fmt.allocPrint(w.gpa, "{s} < in{d}.txt", .{ w.lang.run, k }), 5) catch |err| switch (err) {
            error.Timeout => {
                try r.print("{s}✗ {d}  time limit (5 s){s}\n", .{ red, k, reset });
                passed = false;
                continue;
            },
            else => return err,
        };
        const ms = start.durationTo(.now(w.io, .awake)).toMilliseconds();

        if (!run.term.success()) {
            try r.print("{s}✗ {d}  runtime error{s}  {d} ms\n{s}\n", .{ red, k, reset, ms, run.stderr });
            passed = false;
        } else if (equal(run.stdout, answer)) {
            try r.print("{s}✓ {d}{s}  {d} ms\n", .{ green, k, reset, ms });
        } else {
            try r.print("{s}✗ {d}  wrong answer{s}  {d} ms\n{s}expected{s}\n{s}{s}got{s}\n{s}\n", .{ red, k, reset, ms, dim, reset, answer, dim, reset, run.stdout });
            passed = false;
        }
    }
    if (k == 1) try r.writeAll("no samples\n");
    w.passed = passed and k > 1;
}

// Codeforces' default checker compares whitespace-separated tokens.
fn equal(got: []const u8, want: []const u8) bool {
    var a = std.mem.tokenizeAny(u8, got, " \t\r\n");
    var b = std.mem.tokenizeAny(u8, want, " \t\r\n");
    while (true) {
        const x = a.next();
        const y = b.next();
        if (x == null or y == null) return x == null and y == null;
        if (!std.mem.eql(u8, x.?, y.?)) return false;
    }
}

fn copy(w: *Watch) !void {
    const source = try Io.Dir.cwd().readFileAlloc(w.io, try std.fmt.allocPrint(w.gpa, "{s}/{s}", .{ try w.dir(), w.lang.submit }), w.gpa, .unlimited);
    var child = try std.process.spawn(w.io, .{ .argv = &.{"pbcopy"}, .stdin = .pipe });
    try child.stdin.?.writeStreamingAll(w.io, source);
    child.stdin.?.close(w.io);
    child.stdin = null;
    _ = try child.wait(w.io);
    w.verdict = "copied";
    w.expect();
}

fn submit(w: *Watch) !void {
    var child = try std.process.spawn(w.io, .{ .argv = &.{ "open", try w.url("submit?submittedProblemIndex=") } });
    _ = try child.wait(w.io);
    w.expect();
}

fn expect(w: *Watch) void {
    if (w.pending == null) w.pending = Io.Timestamp.now(w.io, .real).toSeconds();
}

fn poll(w: *Watch) !void {
    const pending = w.pending.?;
    if (Io.Timestamp.now(w.io, .real).toSeconds() - pending > 15 * 60) {
        w.pending = null;
        return;
    }
    const latest = w.cf.latest(w.config.handle) catch return;
    for (latest) |sub| {
        if (sub.creationTimeSeconds < pending - 30 or !w.same(sub.problem, w.ids[w.current])) continue;
        const verdict = sub.verdict orelse "IN_QUEUE";
        w.verdict = if (std.mem.eql(u8, verdict, "OK"))
            green ++ "Accepted" ++ reset ++ "  n next"
        else if (std.mem.eql(u8, verdict, "TESTING") or std.mem.eql(u8, verdict, "IN_QUEUE"))
            try std.fmt.allocPrint(w.gpa, "testing… {d} passed", .{sub.passedTestCount})
        else
            try std.fmt.allocPrint(w.gpa, "{s}{s} on test {d}{s}", .{ red, verdict, sub.passedTestCount + 1, reset });
        if (sub.verdict != null and !std.mem.eql(u8, verdict, "TESTING")) {
            w.submissions = try std.mem.concat(w.gpa, codeforces.Submission, &.{ &.{sub}, w.submissions });
            w.pending = null;
        }
        return;
    }
    w.verdict = "waiting for your submission…";
}

fn write(w: *Watch, bytes: []const u8) !void {
    try Io.File.stdout().writeStreamingAll(w.io, bytes);
}

fn render(w: *Watch) !void {
    var out: Io.Writer.Allocating = .init(w.gpa);
    const r = &out.writer;
    try r.writeAll("\x1b[H\x1b[2J");
    if (w.cursor) |cursor| {
        try r.print("{s}{s}{s}  {d}/{d}\n\n", .{ bold, w.path, reset, w.solved(), w.ids.len });
        const top = cursor -| 10;
        for (w.ids[top..@min(top + 20, w.ids.len)], top..) |id, i| {
            const problem = w.catalog.get(id);
            const mark = switch (w.status(id)) {
                .solved => green ++ "✓" ++ reset,
                .tried => red ++ "✗" ++ reset,
                .todo => dim ++ "·" ++ reset,
            };
            try r.print("{s} {s} {s:<8} {s:<48}", .{ if (i == cursor) ">" else " ", mark, id, if (problem) |p| p.name else "" });
            if (problem) |p| if (p.rating) |rating| try r.print("  {s}{d}{s}", .{ dim, rating, reset });
            try r.writeByte('\n');
        }
        try r.print("\n{s}j/k move · enter open · l back{s}\n", .{ dim, reset });
        return w.write(out.written());
    }

    const id = w.ids[w.current];
    const problem_url = try w.url("problem/");
    try r.print("{s}{s}{s}", .{ bold, id, reset });
    if (w.catalog.get(id)) |p| {
        try r.print(" · {s}", .{p.name});
        if (p.rating) |rating| try r.print(" · {d}", .{rating});
        if (p.tags.len > 0) try r.print(" {s}· {s}", .{ dim, try std.mem.join(w.gpa, ", ", p.tags) });
    }
    try r.print("{s}\n{s}{d}/{d} in {s}{s}\n", .{ reset, dim, w.solved(), w.ids.len, w.path, reset });
    // OSC 8 makes the link clickable in terminals that support it.
    try r.print("\x1b]8;;{s}\x1b\\{s}\x1b]8;;\x1b\\\n", .{ problem_url, problem_url });
    try r.print("{s}/main.{s}\n\n{s}\n", .{ try w.dir(), w.lang.ext, w.report });
    if (w.passed) try r.print("{s}samples ok{s} · submit {s} as {s}\n", .{ green, reset, w.lang.submit, w.lang.judge });
    if (w.verdict.len > 0) try r.print("{s}\n", .{w.verdict});
    try r.print("\n{s}y copy · o submit page · l list · n next · r rerun · q quit{s}\n", .{ dim, reset });
    try w.write(out.written());
}
