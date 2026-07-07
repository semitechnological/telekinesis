const std = @import("std");

const log = std.log.scoped(.provider);

pub const ProviderId = enum {
    openai,
    anthropic,
    google,
    local,
};

pub const Provider = struct {
    id: ProviderId,
    base_url: []const u8,
    api_key: ?[]const u8,

    pub fn defaultUrl(id: ProviderId) []const u8 {
        return switch (id) {
            .openai => "https://api.openai.com/v1",
            .anthropic => "https://api.anthropic.com/v1",
            .google => "https://generativelanguage.googleapis.com/v1beta",
            .local => "http://localhost:11434/v1",
        };
    }
};

pub const Registry = struct {
    allocator: std.mem.Allocator,
    providers: std.ArrayList(Provider),

    pub fn init(allocator: std.mem.Allocator) Registry {
        return .{
            .allocator = allocator,
            .providers = std.ArrayList(Provider).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        self.providers.deinit(self.allocator);
    }

    pub fn add(self: *Registry, id: ProviderId) !void {
        try self.providers.append(self.allocator, .{
            .id = id,
            .base_url = Provider.defaultUrl(id),
            .api_key = null,
        });
        log.info("registered provider: {s}", .{@tagName(id)});
    }

    pub fn count(self: *const Registry) usize {
        return self.providers.items.len;
    }

    pub fn get(self: *const Registry, id: ProviderId) ?Provider {
        for (self.providers.items) |provider| {
            if (provider.id == id) return provider;
        }
        return null;
    }
};

test "registry adds providers" {
    const gpa = std.testing.allocator;
    var registry = Registry.init(gpa);
    defer registry.deinit();

    try registry.add(.openai);
    try registry.add(.anthropic);

    try std.testing.expectEqual(@as(usize, 2), registry.count());
    try std.testing.expect(registry.get(.openai) != null);
}
