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
        } else if (std.mem.eql(u8, cmd, "plugin-pi")) {
            try runPluginPiDemo(gpa, io, stdout, args[2..]);
        } else if (std.mem.eql(u8, cmd, "acp")) {
            try runAcpDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "serve")) {
            try runIpcServer(init, gpa, io, stdout, args[2..]);
        } else {
            try stdout.print("Usage: telekinesis <agent|provider|session|lsp|net|plugin|acp|serve>\n", .{});
        }
    } else {
        try stdout.print("Usage: telekinesis <agent|provider|session|lsp|net|plugin|acp|serve>\n", .{});
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

    // Load our example extension
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

    try stdout.print("Commands: {d}\n", .{registry.plugins.items[0].registered_commands.items.len});
    for (registry.plugins.items[0].registered_commands.items) |cmd| {
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

fn runPluginPiDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, ext_args: []const []const u8) !void {
    if (ext_args.len < 1) {
        try stdout.print("Usage: telekinesis plugin-pi <extension-path>\n", .{});
        return;
    }
    const ext_path = ext_args[0];

    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var registry = telekinesis.plugin.Registry.init(gpa, io);
    defer registry.deinit();

    try registry.add("pi-ext", "Pi Extension", ext_path);
    try registry.startAll();

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

    try stdout.print("Pi extension: {s}\n", .{ext_path});
    try stdout.print("Plugins: {d}\n", .{registry.count()});
    const tools = try registry.allTools(gpa);
    defer gpa.free(tools);
    try stdout.print("Registered tools: {d}\n", .{tools.len});
    for (tools) |tool| {
        try stdout.print("  - {s}: {s}\n", .{ tool.name, tool.description });
    }

    try stdout.print("Commands: {d}\n", .{registry.plugins.items[0].registered_commands.items.len});
    for (registry.plugins.items[0].registered_commands.items) |cmd| {
        try stdout.print("  - /{s}: {s}\n", .{ cmd.name, cmd.description });
    }

    // Call the first registered tool with test arguments
    if (tools.len > 0) {
        const tool_name = tools[0].name;
        if (tool_registry.get(tool_name)) |tool| {
            const result = try tool.execute(tool.ctx, gpa, "{}");
            defer gpa.free(result.content);
            try stdout.print("Tool '{s}' result: {s}\n", .{ tool_name, result.content });
            try stdout.print("Tool error: {}\n", .{result.is_error});
        }
    }

    registry.stopAll();
    try stdout.print("Pi extension demo complete.\n", .{});
}

fn runAcpDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var host = telekinesis.acp.Host.init(gpa, io);
    defer host.deinit();

    try stdout.print("ACP host: spawning echo agent...\n", .{});
    const agent_proc = try host.spawn("echo-bot", "echo", &.{"hello from telekinesis acp"});

    try stdout.print("Sending prompt to echo-bot...\n", .{});
    var read_buf: [4096]u8 = undefined;
    const response = agent_proc.promptAndWait("hello", &read_buf) catch |err| {
        try stdout.print("ACP demo: prompt failed: {}\n", .{err});
        try stdout.print("Note: 'echo' doesn't speak ACP; this demo shows the subprocess scaffolding.\n", .{});
        try stdout.print("To test for real, point a real ACP agent (e.g. zed's agent) at --acp.\n", .{});
        return;
    };

    try stdout.print("Response: {s}\n", .{response});
    try stdout.print("ACP demo complete.\n", .{});
}

fn runIpcServer(init: std.process.Init, gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, args: []const []const u8) !void {
    const data_dir = blk: {
        const home = init.environ_map.get("HOME") orelse "/tmp";
        const dir_path = try std.fmt.allocPrint(gpa, "{s}/.telekinesis", .{home});
        std.Io.Dir.cwd().createDirPath(io, dir_path) catch {};
        break :blk dir_path;
    };
    defer gpa.free(data_dir);
    const socket_path = if (args.len > 0) args[0] else try std.fmt.allocPrint(gpa, "{s}/telekinesis.sock", .{data_dir});
    defer if (args.len == 0) gpa.free(socket_path);

    // Load config
    var cfg = blk: {
        const c = telekinesis.config.load(gpa, io, data_dir, init.environ_map) catch |err| {
            try stdout.print("Warning: config load failed: {}, using defaults\n", .{err});
            break :blk telekinesis.config.Config.init(gpa, data_dir);
        };
        break :blk c;
    };
    defer cfg.deinit();

    // Set up provider
    var http_client = telekinesis.provider.Client.init(gpa, io);
    defer http_client.deinit();

    const provider_config = cfg.getProviderConfig();

    // Set up agent
    var agent_instance = telekinesis.Agent.init(gpa, io);
    defer agent_instance.deinit();

    if (provider_config) |pc| {
        agent_instance.setProvider(&http_client, pc);
        agent_instance.model = pc.default_model;
        try stdout.print("Provider: {s} (model: {s})\n", .{ @tagName(pc.id), pc.default_model });
    } else {
        try stdout.print("Warning: no provider configured. Set OPENAI_API_KEY or create {s}/config.json\n", .{data_dir});
    }

    if (cfg.system_prompt) |sp| {
        agent_instance.setSystemPrompt(sp);
    }

    // Register built-in tools
    var tool_registry = telekinesis.ToolRegistry.init(gpa);
    defer tool_registry.deinit();
    telekinesis.tools.setIo(io);
    try telekinesis.tools.registerBuiltins(&tool_registry);
    agent_instance.setTools(&tool_registry);

    // Set up session store
    var session_store = telekinesis.session.Store.init(gpa, io, data_dir) catch |err| {
        try stdout.print("Warning: session store init failed: {}\n", .{err});
        return;
    };
    defer session_store.deinit();

    // Start limbo database helper
    var db_client = telekinesis.db.Client.init(gpa, io);
    errdefer db_client.deinit();

    const db_path = try std.fmt.allocPrint(gpa, "{s}/session.db", .{data_dir});
    defer gpa.free(db_path);

    const db_helper_name = "telekinesis-db";

    db_client.start(db_path, db_helper_name) catch |err| {
        try stdout.print("Warning: db helper start failed: {}. Session persistence will use JSONL fallback.\n", .{err});
    };
    session_store.attachDb(&db_client);

    var plugin_registry = telekinesis.plugin.Registry.init(gpa, io);
    defer plugin_registry.deinit();

    // Set up ACP host for subagent spawning
    var acp_host = telekinesis.acp.Host.init(gpa, io);
    defer acp_host.deinit();

    var server = telekinesis.ipc.Server.init(gpa, io, socket_path);
    defer server.deinit();
    server.attachAgent(&agent_instance);
    server.attachTools(&tool_registry);
    server.attachPlugins(&plugin_registry);
    server.attachSessionStore(&session_store);
    server.attachAcpHost(&acp_host);

    try stdout.print("Telekinesis IPC server starting on {s}\n", .{socket_path});
    try stdout.print("Database: {s} (limbo)\n", .{db_path});
    try stdout.flush();

    try server.run();
}

pub const std_options: std.Options = .{
    .log_level = .info,
};
