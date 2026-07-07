const std = @import("std");
const telekinesis = @import("telekinesis");

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;

    var stdout_buffer: [4096]u8 = undefined;
    var stdout_file_writer: std.Io.File.Writer = .init(.stdout(), io, &stdout_buffer);
    const stdout = &stdout_file_writer.interface;

    try telekinesis.printBanner(stdout);

    const args = try init.minimal.args.toSlice(init.arena.allocator());
    if (args.len > 1) {
        const cmd = args[1];
        if (std.mem.eql(u8, cmd, "agent")) {
            try runAgentDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "provider")) {
            try runProviderDemo(gpa, stdout);
        } else if (std.mem.eql(u8, cmd, "session")) {
            try runSessionDemo(gpa, stdout);
        } else if (std.mem.eql(u8, cmd, "lsp")) {
            try runLspDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "net")) {
            try runNetDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "plugin")) {
            try runPluginDemo(gpa, io, stdout);
        } else {
            try stdout.print("Usage: telekinesis <agent|provider|session|lsp|net|plugin>\n", .{});
        }
    } else {
        try stdout.print("Usage: telekinesis <agent|provider|session|lsp|net|plugin>\n", .{});
    }

    try stdout.flush();
}

fn runAgentDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent = telekinesis.Agent.init(arena, io);
    defer agent.deinit();

    agent.subscribe(null, onEvent) catch {};
    try agent.prompt("Hello, Telekinesis!");

    try stdout.print("Agent demo complete.\n", .{});
}

fn onEvent(ctx: ?*anyopaque, event: telekinesis.Event) void {
    _ = ctx;
    std.log.info("event: {}", .{event});
}

fn runProviderDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var providers = telekinesis.ProviderRegistry.init(gpa);
    defer providers.deinit();

    try providers.add(.openai);
    try providers.add(.anthropic);

    try stdout.print("Providers: {d}\n", .{providers.count()});
}

fn runSessionDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    const session = telekinesis.Session;
    var s = try session.init(gpa, "demo", "demo session");
    defer s.deinit();

    _ = try s.append(.user, "hello from session demo");
    _ = try s.append(.assistant, "session demo received");

    try stdout.print("Session entries: {d}\n", .{s.messageCount()});
}

fn runLspDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var manager = telekinesis.lsp.Manager.init(gpa, io);
    defer manager.deinit();

    const langs = try manager.supportedLanguages(gpa);
    defer gpa.free(langs);

    try stdout.print("Supported LSP languages: {d}\n", .{langs.len});
    for (langs) |lang| {
        try stdout.print("  - {s}\n", .{lang});
    }
}

fn runNetDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    const device_id = telekinesis.net.generateDeviceId(io);
    var client = try telekinesis.net.SignalingClient.init(gpa, io, "https://signal.telekinesis.dev", device_id, "local");
    defer client.deinit();

    try client.announce();
    try stdout.print("Device announced: {x}\n", .{device_id});
}

fn runPluginDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var registry = telekinesis.plugin.Registry.init(gpa, io);
    defer registry.deinit();

    try registry.add("example", "Example Plugin", "plugins/example.ts");
    try registry.startAll();

    // Block on reading messages until plugin is ready
    while (!registry.plugins.items[0].ready) {
        const msg = try registry.plugins.items[0].readMessage(arena);
        if (msg == null) break;
        try registry.plugins.items[0].processMessage(msg.?);
    }

    var tool_registry = telekinesis.ToolRegistry.init(gpa);
    defer tool_registry.deinit();

    var bridge = telekinesis.plugin.PluginToolBridge.init(&registry, arena);
    defer bridge.deinit();
    try bridge.registerPluginToolsToAgent(&tool_registry);

    try stdout.print("Plugins: {d}\n", .{registry.count()});
    const tools = try registry.allTools(gpa);
    defer gpa.free(tools);
    try stdout.print("Registered tools: {d}\n", .{tools.len});
    for (tools) |tool| {
        try stdout.print("  - {s}: {s}\n", .{ tool.name, tool.description });
    }

    const commands = try registry.plugins.items[0].registered_commands.toOwnedSlice(gpa);
    defer gpa.free(commands);
    try stdout.print("Commands: {d}\n", .{commands.len});
    for (commands) |cmd| {
        try stdout.print("  - /{s}: {s}\n", .{ cmd.name, cmd.description });
    }

    if (tool_registry.get("word_count")) |tool| {
        const result = try tool.execute(tool.ctx, gpa, "{\"text\":\"hello world from telekinesis\"}");
        defer gpa.free(result.content);
        try stdout.print("Tool call result: {s}\n", .{result.content});
        try stdout.print("Tool error: {}\n", .{result.is_error});
    }

    try registry.forwardEvent("turn_start", null);

    registry.stopAll();
    try stdout.print("Plugin demo complete.\n", .{});
}

pub const std_options: std.Options = .{
    .log_level = .info,
};
