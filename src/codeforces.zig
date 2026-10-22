const std = @import("std");
const Io = std.Io;
const Allocator = std.mem.Allocator;

pub const Problem = struct {
    contestId: u32 = 0,
    index: []const u8,
    name: []const u8 = "",
    rating: ?u32 = null,
    tags: []const []const u8 = &.{},
};

pub const Submission = struct {
    creationTimeSeconds: i64,
    problem: Problem,
    verdict: ?[]const u8 = null,
    passedTestCount: u32 = 0,
};

const Problemset = struct { problems: []Problem };

fn Response(T: type) type {
    return struct { result: T };
}

pub fn id(gpa: Allocator, problem: Problem) ![]const u8 {
    return std.fmt.allocPrint(gpa, "{d}{s}", .{ problem.contestId, problem.index });
}

pub const Client = struct {
    gpa: Allocator,
    io: Io,
    http: std.http.Client,
    cache: Io.Dir,
    last: Io.Timestamp = .zero,

    pub fn init(gpa: Allocator, io: Io, environ: *std.process.Environ.Map) !Client {
        const base = environ.get("XDG_CACHE_HOME") orelse try std.fs.path.join(gpa, &.{ environ.get("HOME").?, ".cache" });
        return .{
            .gpa = gpa,
            .io = io,
            .http = .{ .allocator = gpa, .io = io },
            .cache = try Io.Dir.cwd().createDirPathOpen(io, try std.fs.path.join(gpa, &.{ base, "cf" }), .{}),
        };
    }

    pub fn get(c: *Client, url: []const u8) ![]u8 {
        var body: Io.Writer.Allocating = .init(c.gpa);
        // Without a browser user agent Cloudflare answers problem pages with a challenge.
        const res = try c.http.fetch(.{
            .location = .{ .url = url },
            .response_writer = &body.writer,
            .headers = .{ .user_agent = .{ .override = "Mozilla/5.0" } },
        });
        if (res.status != .ok) return error.BadStatus;
        return body.written();
    }

    // The API allows one call every two seconds.
    fn api(c: *Client, comptime T: type, url: []const u8) !struct { T, []u8 } {
        const wait = Io.Duration.fromSeconds(2).nanoseconds - c.last.durationTo(.now(c.io, .awake)).nanoseconds;
        if (wait > 0) try c.io.sleep(.fromNanoseconds(wait), .awake);
        c.last = .now(c.io, .awake);
        const body = try c.get(url);
        const res = try std.json.parseFromSliceLeaky(Response(T), c.gpa, body, .{ .ignore_unknown_fields = true });
        return .{ res.result, body };
    }

    fn cached(c: *Client, comptime T: type, name: []const u8) ?T {
        const body = c.cache.readFileAlloc(c.io, name, c.gpa, .unlimited) catch return null;
        return (std.json.parseFromSliceLeaky(Response(T), c.gpa, body, .{ .ignore_unknown_fields = true }) catch return null).result;
    }

    pub fn problems(c: *Client, refresh: bool) ![]Problem {
        if (!refresh) if (c.cached(Problemset, "problems.json")) |set| return set.problems;
        const set, const body = try c.api(Problemset, "https://codeforces.com/api/problemset.problems");
        try c.cache.writeFile(c.io, .{ .sub_path = "problems.json", .data = body });
        return set.problems;
    }

    pub fn submissions(c: *Client, handle: []const u8) ![]Submission {
        const name = try std.fmt.allocPrint(c.gpa, "status-{s}.json", .{handle});
        const url = try std.fmt.allocPrint(c.gpa, "https://codeforces.com/api/user.status?handle={s}", .{handle});
        const subs, const body = c.api([]Submission, url) catch return c.cached([]Submission, name) orelse error.Offline;
        try c.cache.writeFile(c.io, .{ .sub_path = name, .data = body });
        return subs;
    }

    pub fn latest(c: *Client, handle: []const u8) ![]Submission {
        const url = try std.fmt.allocPrint(c.gpa, "https://codeforces.com/api/user.status?handle={s}&count=5", .{handle});
        const subs, _ = try c.api([]Submission, url);
        return subs;
    }
};
