const std = @import("std");
const Allocator = std.mem.Allocator;

pub const Sample = struct { input: []const u8, answer: []const u8 };

pub const Page = struct {
    title: []const u8,
    limits: []const u8,
    body: []const u8,
    samples: []Sample,
};

// Codeforces statements write math as $$$...$$$ LaTeX; these are the macros that show up in practice.
const symbols = [_][2][]const u8{
    .{ "<br />", "\n" }, .{ "</p>", "\n\n" }, .{ "</div>", "\n" }, .{ "<li>", "- " },
    .{ "</li>", "\n" },  .{ "$$$", "" },      .{ "\\left", "" },   .{ "\\right", "" },
    .{ "\\leq", "≤" },
    .{ "\\le", "≤" },
    .{ "\\geq", "≥" },
    .{ "\\ge", "≥" },
    .{ "\\neq", "≠" },
    .{ "\\ne", "≠" },
    .{ "\\cdot", "·" },
    .{ "\\ldots", "…" },
    .{ "\\dots", "…" },
    .{ "\\times", "×" },
    .{ "\\to", "→" },
    .{ "\\oplus", "⊕" },
    .{ "\\lfloor", "⌊" },
    .{ "\\rfloor", "⌋" },
    .{ "\\lceil", "⌈" },
    .{ "\\rceil", "⌉" },
    .{ "\\sum", "Σ" },
    .{ "\\,", " " },     .{ "\\texttt", "" }, .{ "\\mathrm", "" }, .{ "\\operatorname", "" },
};

const entities = [_][2][]const u8{
    .{ "&lt;", "<" }, .{ "&gt;", ">" }, .{ "&quot;", "\"" }, .{ "&#39;", "'" }, .{ "&nbsp;", " " }, .{ "&amp;", "&" },
};

pub fn ids(gpa: Allocator, source: []const u8) ![]const []const u8 {
    var found: std.array_hash_map.String(void) = .empty;
    var words = std.mem.tokenizeAny(u8, source, " \t\r\n,;\"'<>()[]");
    while (words.next()) |word| {
        if (link(gpa, word)) |problem| try found.put(gpa, try problem, {});
        if (bare(word)) try found.put(gpa, word, {});
    }
    return found.keys();
}

fn link(gpa: Allocator, word: []const u8) ?Allocator.Error![]const u8 {
    var parts = std.mem.splitScalar(u8, word, '/');
    var previous: [3][]const u8 = .{ "", "", "" };
    while (parts.next()) |part| {
        if (std.mem.eql(u8, previous[1], "problemset") and std.mem.eql(u8, previous[0], "problem") and digits(part))
            return std.fmt.allocPrint(gpa, "{s}{s}", .{ part, parts.next() orelse return null });
        if (std.mem.eql(u8, previous[2], "contest") and digits(previous[1]) and std.mem.eql(u8, previous[0], "problem"))
            return std.fmt.allocPrint(gpa, "{s}{s}", .{ previous[1], part });
        const shifted: [3][]const u8 = .{ part, previous[0], previous[1] };
        previous = shifted;
    }
    return null;
}

fn digits(s: []const u8) bool {
    for (s) |c| if (!std.ascii.isDigit(c)) return false;
    return s.len > 0;
}

fn bare(word: []const u8) bool {
    const split = std.mem.indexOfNone(u8, word, "0123456789") orelse return false;
    const index = word[split..];
    return split > 0 and index.len <= 2 and std.ascii.isUpper(index[0]) and (index.len == 1 or std.ascii.isDigit(index[1]));
}

fn between(s: []const u8, start: []const u8, end: []const u8) []const u8 {
    const i = (std.mem.indexOf(u8, s, start) orelse return "") + start.len;
    return s[i .. std.mem.indexOfPos(u8, s, i, end) orelse s.len];
}

fn text(gpa: Allocator, html: []const u8) ![]const u8 {
    var s = html;
    for (symbols) |pair| s = try std.mem.replaceOwned(u8, gpa, s, pair[0], pair[1]);
    var plain: std.ArrayList(u8) = .empty;
    var tag = false;
    for (s) |c| {
        if (c == '<') tag = true;
        if (!tag) try plain.append(gpa, c);
        if (c == '>') tag = false;
    }
    s = plain.items;
    for (entities) |pair| s = try std.mem.replaceOwned(u8, gpa, s, pair[0], pair[1]);
    return s;
}

pub fn parse(gpa: Allocator, html: []const u8) !Page {
    const problem = between(html, "class=\"problem-statement\">", "<script");
    const header_end = std.mem.indexOf(u8, problem, "class=\"output-file") orelse 0;
    const after = std.mem.indexOfPos(u8, problem, header_end, "</div></div>") orelse 0;
    // Statement HTML breaks lines mid-sentence; paragraphs come from the tags.
    const body = try std.mem.replaceOwned(u8, gpa, between(problem[after..], "</div></div>", "<div class=\"sample-tests\">"), "\n", " ");

    var samples: std.ArrayList(Sample) = .empty;
    var rest = problem;
    while (std.mem.indexOf(u8, rest, "<div class=\"input\">")) |i| {
        rest = rest[i..];
        const input = between(rest, "<pre>", "</pre>");
        rest = rest[std.mem.indexOf(u8, rest, "<div class=\"output\">") orelse break ..];
        const answer = between(rest, "<pre>", "</pre>");
        try samples.append(gpa, .{
            .input = try trimmed(gpa, try text(gpa, input)),
            .answer = try trimmed(gpa, try text(gpa, answer)),
        });
        rest = rest[1..];
    }

    return .{
        .title = try text(gpa, between(problem, "<div class=\"title\">", "</div>")),
        .limits = try std.fmt.allocPrint(gpa, "{s} · {s}", .{
            between(problem, "time limit per test</div>", "</div>"),
            between(problem, "memory limit per test</div>", "</div>"),
        }),
        .body = try text(gpa, body),
        .samples = samples.items,
    };
}

fn trimmed(gpa: Allocator, s: []const u8) ![]const u8 {
    return std.fmt.allocPrint(gpa, "{s}\n", .{std.mem.trim(u8, s, " \r\n")});
}

pub fn comment(gpa: Allocator, prefix: []const u8, page: Page, url: []const u8) ![]const u8 {
    var out: std.Io.Writer.Allocating = .init(gpa);
    const w = &out.writer;
    try w.print("{s} {s}\n{s} {s}\n{s} {s}\n{s}\n", .{ prefix, page.title, prefix, page.limits, prefix, url, prefix });
    var blank = false;
    var lines = std.mem.splitScalar(u8, page.body, '\n');
    while (lines.next()) |raw| {
        const line = std.mem.trim(u8, raw, " \t\r");
        if (line.len == 0) {
            if (!blank) try w.print("{s}\n", .{prefix});
            blank = true;
            continue;
        }
        blank = false;
        var column: usize = 0;
        var words = std.mem.tokenizeScalar(u8, line, ' ');
        while (words.next()) |word| {
            if (column > 0 and column + word.len > 76) {
                try w.writeByte('\n');
                column = 0;
            }
            if (column == 0) try w.writeAll(prefix);
            try w.print(" {s}", .{word});
            column += word.len + 1;
        }
        try w.writeByte('\n');
    }
    try w.writeByte('\n');
    return out.written();
}

test "ids" {
    const gpa = std.testing.allocator;
    var arena: std.heap.ArenaAllocator = .init(gpa);
    defer arena.deinit();
    const found = try ids(arena.allocator(),
        \\4A 1352C, junk 1200 A
        \\<a href="https://codeforces.com/problemset/problem/1374/B">x</a>
        \\https://codeforces.com/contest/1520/problem/F2 4A
    );
    try std.testing.expectEqual(4, found.len);
    try std.testing.expectEqualStrings("4A", found[0]);
    try std.testing.expectEqualStrings("1352C", found[1]);
    try std.testing.expectEqualStrings("1374B", found[2]);
    try std.testing.expectEqualStrings("1520F2", found[3]);
}
