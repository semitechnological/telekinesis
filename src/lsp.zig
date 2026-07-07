const std = @import("std");

const log = std.log.scoped(.lsp);

pub const Language = enum {
    zig,
    rust,
    go,
    typescript,
};

pub const Client = struct {
    language: Language,
    command: []const u8,

    pub fn init(language: Language, command: []const u8) Client {
        return .{
            .language = language,
            .command = command,
        };
    }

    pub fn start(self: *Client) !void {
        log.info("starting LSP client for {s}: {s}", .{ @tagName(self.language), self.command });
    }
};

test "lsp client can start" {
    var client = Client.init(.zig, "zls");
    try client.start();
}
