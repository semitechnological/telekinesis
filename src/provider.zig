const std = @import("std");

const log = std.log.scoped(.provider);

pub const ProviderId = enum {
    openai,
    anthropic,
    google,
    local,

    pub fn defaultUrl(id: ProviderId) []const u8 {
        return switch (id) {
            .openai => "https://api.openai.com/v1",
            .anthropic => "https://api.anthropic.com/v1",
            .google => "https://generativelanguage.googleapis.com/v1beta",
            .local => "http://localhost:11434/v1",
        };
    }

    pub fn defaultModel(id: ProviderId) []const u8 {
        return switch (id) {
            .openai => "gpt-4o",
            .anthropic => "claude-sonnet-4-20250514",
            .google => "gemini-2.0-flash",
            .local => "llama3.1",
        };
    }
};

pub const Role = enum {
    user,
    assistant,
    system,
    tool,

    pub fn jsonStringify(self: Role, jw: *std.json.Stringify) !void {
        try jw.write(@tagName(self));
    }
};

pub const Message = struct {
    role: Role,
    content: []const u8,
};

pub const ToolDef = struct {
    type: []const u8 = "function",
    function: ToolFunction,
};

pub const ToolFunction = struct {
    name: []const u8,
    description: []const u8,
    parameters: []const u8,
};

pub const ChatRequest = struct {
    model: []const u8,
    messages: []const Message,
    stream: bool = false,
    temperature: ?f32 = null,
    max_tokens: ?u32 = null,
    tools: ?[]const ToolDef = null,
};

pub const ResponseToolCall = struct {
    id: []const u8 = "",
    type: []const u8 = "function",
    function: ResponseToolFunction = .{},
};

pub const ResponseToolFunction = struct {
    name: []const u8 = "",
    arguments: []const u8 = "",
};

pub const ChatChoice = struct {
    index: u32 = 0,
    message: ChatResponseMessage,
    finish_reason: ?[]const u8 = null,
};

pub const ChatResponseMessage = struct {
    role: Role,
    content: []const u8 = "",
    tool_calls: ?[]const ResponseToolCall = null,
};

pub const ChatResponse = struct {
    id: []const u8 = "",
    model: []const u8 = "",
    choices: []const ChatChoice = &.{},
};

pub const StreamDelta = struct {
    role: ?Role = null,
    content: ?[]const u8 = null,
};

pub const StreamChoice = struct {
    index: u32 = 0,
    delta: StreamDelta,
    finish_reason: ?[]const u8 = null,
};

pub const StreamChunk = struct {
    id: []const u8 = "",
    choices: []const StreamChoice = &.{},
};

pub const Provider = struct {
    id: ProviderId,
    base_url: []const u8,
    api_key: ?[]const u8,
    default_model: []const u8,
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
            .base_url = ProviderId.defaultUrl(id),
            .api_key = null,
            .default_model = ProviderId.defaultModel(id),
        });
        log.info("registered provider: {s}", .{@tagName(id)});
    }

    pub fn addWithKey(self: *Registry, id: ProviderId, api_key: []const u8) !void {
        try self.providers.append(self.allocator, .{
            .id = id,
            .base_url = ProviderId.defaultUrl(id),
            .api_key = try self.allocator.dupe(u8, api_key),
            .default_model = ProviderId.defaultModel(id),
        });
        log.info("registered provider: {s} (with key)", .{@tagName(id)});
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

pub const HttpError = error{
    NetworkFailure,
    HttpStatusError,
    InvalidResponse,
    OutOfMemory,
    StreamParseError,
};

pub const Client = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    http_client: std.http.Client,

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Client {
        return .{
            .allocator = allocator,
            .io = io,
            .http_client = .{
                .allocator = allocator,
                .io = io,
            },
        };
    }

    pub fn deinit(self: *Client) void {
        self.http_client.deinit();
    }

    pub fn chatCompletion(
        self: *Client,
        provider: Provider,
        request: ChatRequest,
    ) !ChatResponse {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        var body_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const body_writer = &body_buf.writer;
        defer body_buf.deinit();

        try std.json.Stringify.value(request, .{}, body_writer);

        const url = try std.fmt.allocPrint(arena_alloc, "{s}/chat/completions", .{provider.base_url});

        var auth_header_buf: [256]u8 = undefined;
        const auth_header: ?std.http.Header = if (provider.api_key) |key| blk: {
            const value = try std.fmt.bufPrint(&auth_header_buf, "Bearer {s}", .{key});
            break :blk .{ .name = "Authorization", .value = value };
        } else null;

        var extra_headers: [2]std.http.Header = undefined;
        var header_count: usize = 0;
        extra_headers[header_count] = .{ .name = "Content-Type", .value = "application/json" };
        header_count += 1;
        if (auth_header) |ah| {
            extra_headers[header_count] = ah;
            header_count += 1;
        }

        var response_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const response_writer = &response_buf.writer;
        defer response_buf.deinit();

        const result = self.http_client.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body_buf.written(),
            .extra_headers = extra_headers[0..header_count],
            .response_writer = response_writer,
        }) catch |err| {
            log.err("HTTP request failed: {}", .{err});
            return error.NetworkFailure;
        };

        if (result.status.class() != .success) {
            log.err("HTTP {d}: {s}", .{ @intFromEnum(result.status), response_buf.written() });
            return error.HttpStatusError;
        }

        const parsed = std.json.parseFromSlice(ChatResponse, arena_alloc, response_buf.written(), .{
            .ignore_unknown_fields = true,
        }) catch |err| {
            log.err("JSON parse failed: {}", .{err});
            return error.InvalidResponse;
        };

        return parsed.value;
    }

    pub const StreamCallback = *const fn (ctx: ?*anyopaque, delta: []const u8) void;

    pub fn chatCompletionStream(
        self: *Client,
        provider: Provider,
        request: ChatRequest,
        ctx: ?*anyopaque,
        on_delta: StreamCallback,
    ) !void {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        var req = request;
        req.stream = true;

        var body_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const body_writer = &body_buf.writer;
        defer body_buf.deinit();

        try std.json.Stringify.value(req, .{}, body_writer);

        const url = try std.fmt.allocPrint(arena_alloc, "{s}/chat/completions", .{provider.base_url});

        var auth_header_buf: [256]u8 = undefined;
        const auth_header: ?std.http.Header = if (provider.api_key) |key| blk: {
            const value = try std.fmt.bufPrint(&auth_header_buf, "Bearer {s}", .{key});
            break :blk .{ .name = "Authorization", .value = value };
        } else null;

        var extra_headers: [2]std.http.Header = undefined;
        var header_count: usize = 0;
        extra_headers[header_count] = .{ .name = "Content-Type", .value = "application/json" };
        header_count += 1;
        if (auth_header) |ah| {
            extra_headers[header_count] = ah;
            header_count += 1;
        }

        var http_req = self.http_client.request(.POST, try std.Uri.parse(url), .{
            .extra_headers = extra_headers[0..header_count],
        }) catch |err| {
            log.err("HTTP request setup failed: {}", .{err});
            return error.NetworkFailure;
        };
        defer http_req.deinit();

        http_req.transfer_encoding = .{ .content_length = body_buf.written().len };
        var body_writer_req = http_req.sendBodyUnflushed(&.{}) catch |err| {
            log.err("HTTP send body failed: {}", .{err});
            return error.NetworkFailure;
        };
        try body_writer_req.writer.writeAll(body_buf.written());
        try body_writer_req.end();
        try http_req.connection.?.flush();

        var redirect_buf: [8192]u8 = undefined;
        var response = http_req.receiveHead(&redirect_buf) catch |err| {
            log.err("HTTP receive head failed: {}", .{err});
            return error.NetworkFailure;
        };

        if (response.head.status.class() != .success) {
            log.err("HTTP {d}", .{@intFromEnum(response.head.status)});
            return error.HttpStatusError;
        }

        var read_buf: [4096]u8 = undefined;
        const reader = response.reader(&read_buf);

        while (true) {
            const line = reader.takeDelimiter('\n') catch |err| {
                log.err("stream read failed: {}", .{err});
                return error.StreamParseError;
            };
            if (line == null) break;

            const trimmed = std.mem.trimEnd(u8, line.?, "\r");
            if (trimmed.len > 0) {
                try processSseLine(arena_alloc, trimmed, ctx, on_delta);
            }
        }
    }
};

fn processSseLine(
    arena: std.mem.Allocator,
    line: []const u8,
    ctx: ?*anyopaque,
    on_delta: Client.StreamCallback,
) !void {
    if (line.len == 0) return;
    if (std.mem.startsWith(u8, line, ":")) return;
    if (!std.mem.startsWith(u8, line, "data: ")) return;

    const data = line[6..];
    if (std.mem.eql(u8, data, "[DONE]")) return;

    const parsed = std.json.parseFromSlice(StreamChunk, arena, data, .{
        .ignore_unknown_fields = true,
    }) catch return;

    for (parsed.value.choices) |choice| {
        if (choice.delta.content) |content| {
            on_delta(ctx, content);
        }
    }
}

pub fn serializeChatRequest(allocator: std.mem.Allocator, request: ChatRequest) ![]u8 {
    var buf: std.Io.Writer.Allocating = .init(allocator);
    const writer = &buf.writer;
    defer buf.deinit();
    try std.json.Stringify.value(request, .{}, writer);
    return try allocator.dupe(u8, buf.written());
}

test "registry adds providers" {
    const gpa = std.testing.allocator;
    var registry = Registry.init(gpa);
    defer registry.deinit();

    try registry.add(.openai);
    try registry.add(.anthropic);

    try std.testing.expectEqual(@as(usize, 2), registry.count());
    try std.testing.expect(registry.get(.openai) != null);
}

test "serialize chat request" {
    const gpa = std.testing.allocator;
    const messages = [_]Message{
        .{ .role = .system, .content = "You are helpful." },
        .{ .role = .user, .content = "Hello" },
    };
    const req = ChatRequest{
        .model = "gpt-4o",
        .messages = &messages,
        .stream = false,
    };
    const json = try serializeChatRequest(gpa, req);
    defer gpa.free(json);

    try std.testing.expect(std.mem.indexOf(u8, json, "\"model\":\"gpt-4o\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"role\":\"user\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"content\":\"Hello\"") != null);
}

test "parse stream chunk" {
    const gpa = std.testing.allocator;
    const data =
        \\{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}
    ;
    const parsed = try std.json.parseFromSlice(StreamChunk, gpa, data, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    try std.testing.expectEqual(@as(usize, 1), parsed.value.choices.len);
    try std.testing.expectEqualStrings("Hi", parsed.value.choices[0].delta.content.?);
}

test "process sse line extracts content" {
    const gpa = std.testing.allocator;
    var arena = std.heap.ArenaAllocator.init(gpa);
    defer arena.deinit();

    var captured: []const u8 = "";
    const ctx = &captured;
    const cb = struct {
        fn fn_(c: ?*anyopaque, delta: []const u8) void {
            const ptr: *[]const u8 = @ptrCast(@alignCast(c.?));
            ptr.* = delta;
        }
    }.fn_;

    try processSseLine(arena.allocator(),
        \\data: {"choices":[{"delta":{"content":"world"}}]}
    , ctx, cb);
    try std.testing.expectEqualStrings("world", captured);
}
