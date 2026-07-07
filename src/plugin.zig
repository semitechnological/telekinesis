const std = @import("std");

const log = std.log.scoped(.plugin);

pub const Plugin = struct {
    id: []const u8,
    name: []const u8,
    path: []const u8,
};

pub const Registry = struct {
    allocator: std.mem.Allocator,
    plugins: std.ArrayList(Plugin),

    pub fn init(allocator: std.mem.Allocator) Registry {
        return .{
            .allocator = allocator,
            .plugins = std.ArrayList(Plugin).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        self.plugins.deinit(self.allocator);
    }

    pub fn add(self: *Registry, id: []const u8, name: []const u8, path: []const u8) !void {
        try self.plugins.append(self.allocator, .{
            .id = try self.allocator.dupe(u8, id),
            .name = try self.allocator.dupe(u8, name),
            .path = try self.allocator.dupe(u8, path),
        });
        log.info("loaded plugin: {s}", .{id});
    }
};

test "plugin registry loads plugin" {
    const gpa = std.testing.allocator;
    var registry = Registry.init(gpa);
    defer registry.deinit();
    try registry.add("test", "Test Plugin", "./plugins/test");
    try std.testing.expectEqual(@as(usize, 1), registry.plugins.items.len);
}
