const std = @import("std");
const agent = @import("agent.zig");

const log = std.log.scoped(.session);

pub const SessionId = []const u8;

pub const Entry = struct {
    id: u64,
    session_id: []const u8,
    role: agent.Role,
    content: []const u8,
    parent_id: ?u64 = null,
    tool_call_id: ?[]const u8 = null,
    created_at: i64 = 0,
};

pub const Session = struct {
    allocator: std.mem.Allocator,
    id: []const u8,
    name: []const u8,
    entries: std.ArrayList(Entry),
    next_id: u64 = 1,

    pub fn init(allocator: std.mem.Allocator, id: []const u8, name: []const u8) !Session {
        return .{
            .allocator = allocator,
            .id = try allocator.dupe(u8, id),
            .name = try allocator.dupe(u8, name),
            .entries = std.ArrayList(Entry).empty,
        };
    }

    pub fn deinit(self: *Session) void {
        self.allocator.free(self.id);
        self.allocator.free(self.name);
        for (self.entries.items) |entry| {
            self.allocator.free(entry.session_id);
            self.allocator.free(entry.content);
            if (entry.tool_call_id) |tcid| self.allocator.free(tcid);
        }
        self.entries.deinit(self.allocator);
    }

    pub fn append(self: *Session, role: agent.Role, content: []const u8) !u64 {
        const id = self.next_id;
        self.next_id += 1;
        try self.entries.append(self.allocator, .{
            .id = id,
            .session_id = try self.allocator.dupe(u8, self.id),
            .role = role,
            .content = try self.allocator.dupe(u8, content),
            .parent_id = if (self.entries.items.len > 0) self.entries.items[self.entries.items.len - 1].id else null,
        });
        log.info("session {s}: appended entry {d} ({s})", .{ self.id, id, @tagName(role) });
        return id;
    }

    pub fn appendWithParent(self: *Session, role: agent.Role, content: []const u8, parent_id: u64) !u64 {
        const id = self.next_id;
        self.next_id += 1;
        try self.entries.append(self.allocator, .{
            .id = id,
            .session_id = try self.allocator.dupe(u8, self.id),
            .role = role,
            .content = try self.allocator.dupe(u8, content),
            .parent_id = parent_id,
        });
        return id;
    }

    pub fn fork(self: *Session, allocator: std.mem.Allocator, new_id: []const u8, from_entry_id: u64) !Session {
        var forked = try Session.init(allocator, new_id, self.name);
        errdefer forked.deinit();
        for (self.entries.items) |entry| {
            if (entry.id <= from_entry_id) {
                try forked.entries.append(allocator, .{
                    .id = entry.id,
                    .session_id = try allocator.dupe(u8, new_id),
                    .role = entry.role,
                    .content = try allocator.dupe(u8, entry.content),
                    .parent_id = entry.parent_id,
                    .tool_call_id = if (entry.tool_call_id) |tcid| try allocator.dupe(u8, tcid) else null,
                    .created_at = entry.created_at,
                });
            }
        }
        forked.next_id = self.next_id;
        return forked;
    }

    pub fn messageCount(self: *const Session) usize {
        return self.entries.items.len;
    }

    pub fn getEntry(self: *const Session, id: u64) ?Entry {
        for (self.entries.items) |entry| {
            if (entry.id == id) return entry;
        }
        return null;
    }

    pub fn children(self: *const Session, allocator: std.mem.Allocator, parent_id: u64) ![]u64 {
        var result: std.ArrayList(u64) = .empty;
        for (self.entries.items) |entry| {
            if (entry.parent_id != null and entry.parent_id.? == parent_id) {
                try result.append(allocator, entry.id);
            }
        }
        return result.toOwnedSlice(allocator);
    }
};

pub const Store = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    base_dir: []const u8,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, base_dir: []const u8) !Store {
        try std.Io.Dir.cwd().createDirPath(io, base_dir);
        return .{
            .allocator = allocator,
            .io = io,
            .base_dir = try allocator.dupe(u8, base_dir),
        };
    }

    pub fn deinit(self: *Store) void {
        self.allocator.free(self.base_dir);
    }

    pub fn save(self: *Store, session: *const Session) !void {
        const path = try std.fmt.allocPrint(self.allocator, "{s}/{s}.jsonl", .{ self.base_dir, session.id });
        defer self.allocator.free(path);

        var file = try std.Io.Dir.cwd().createFile(self.io, path, .{});
        defer file.close(self.io);

        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, self.io, &buf);
        const writer = &file_writer.interface;

        for (session.entries.items) |entry| {
            try std.json.Stringify.value(entry, .{}, writer);
            try writer.writeAll("\n");
        }
        try writer.flush();
    }

    pub fn load(self: *Store, id: []const u8) !Session {
        const path = try std.fmt.allocPrint(self.allocator, "{s}/{s}.jsonl", .{ self.base_dir, id });
        defer self.allocator.free(path);

        var file = try std.Io.Dir.cwd().openFile(self.io, path, .{});
        defer file.close(self.io);

        var read_buf: [4096]u8 = undefined;
        var content_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const content_writer = &content_buf.writer;
        defer content_buf.deinit();

        var file_reader = file.readerStreaming(self.io, &read_buf);
        const reader = &file_reader.interface;
        while (true) {
            const n = reader.readSliceShort(&read_buf) catch break;
            if (n == 0) break;
            try content_writer.writeAll(read_buf[0..n]);
        }

        const content = content_buf.written();

        var session = try Session.init(self.allocator, id, "resumed");
        errdefer session.deinit();

        var iter = std.mem.splitScalar(u8, content, '\n');
        var max_id: u64 = 0;
        while (iter.next()) |line| {
            if (line.len == 0) continue;
            const parsed = try std.json.parseFromSlice(Entry, self.allocator, line, .{
                .ignore_unknown_fields = true,
            });
            defer parsed.deinit();

            try session.entries.append(self.allocator, .{
                .id = parsed.value.id,
                .session_id = try self.allocator.dupe(u8, parsed.value.session_id),
                .role = parsed.value.role,
                .content = try self.allocator.dupe(u8, parsed.value.content),
                .parent_id = parsed.value.parent_id,
                .tool_call_id = if (parsed.value.tool_call_id) |tcid| try self.allocator.dupe(u8, tcid) else null,
                .created_at = parsed.value.created_at,
            });
            if (parsed.value.id > max_id) max_id = parsed.value.id;
        }
        session.next_id = max_id + 1;
        return session;
    }
};

test "session append and count" {
    const gpa = std.testing.allocator;
    var session = try Session.init(gpa, "abc", "test");
    defer session.deinit();

    _ = try session.append(.user, "hello");
    _ = try session.append(.assistant, "hi there");

    try std.testing.expectEqual(@as(usize, 2), session.messageCount());
}

test "session fork copies entries up to point" {
    const gpa = std.testing.allocator;
    var session = try Session.init(gpa, "orig", "test");
    defer session.deinit();

    _ = try session.append(.user, "first");
    _ = try session.append(.assistant, "reply");
    const third = try session.append(.user, "second");

    var forked = try session.fork(gpa, "fork1", third);
    defer forked.deinit();

    try std.testing.expectEqual(@as(usize, 3), forked.messageCount());
    try std.testing.expectEqualStrings("second", forked.entries.items[2].content);
}

test "session tree children" {
    const gpa = std.testing.allocator;
    var session = try Session.init(gpa, "tree", "test");
    defer session.deinit();

    const root = try session.append(.user, "root");
    _ = try session.appendWithParent(.assistant, "child1", root);
    _ = try session.appendWithParent(.assistant, "child2", root);

    const kids = try session.children(gpa, root);
    defer gpa.free(kids);

    try std.testing.expectEqual(@as(usize, 2), kids.len);
}

test "store save and load" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    const tmp_dir = ".telekinesis-test-session";
    var store = try Store.init(gpa, io, tmp_dir);
    defer {
        store.deinit();
        std.Io.Dir.cwd().deleteTree(io, tmp_dir) catch {};
    }

    var session = try Session.init(gpa, "persist-test", "test");
    defer session.deinit();

    _ = try session.append(.user, "hello");
    _ = try session.append(.assistant, "world");

    try store.save(&session);

    var loaded = try store.load("persist-test");
    defer loaded.deinit();

    try std.testing.expectEqual(@as(usize, 2), loaded.messageCount());
    try std.testing.expectEqualStrings("hello", loaded.entries.items[0].content);
    try std.testing.expectEqualStrings("world", loaded.entries.items[1].content);
}
