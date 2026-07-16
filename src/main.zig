const std = @import("std");
const telekinesis = @import("telekinesis");

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;

    var stdout_buffer: [4096]u8 = undefined;
    var stdout_file_writer: std.Io.File.Writer = .init(.stdout(), io, &stdout_buffer);
    const stdout = &stdout_file_writer.interface;

    const args = try init.minimal.args.toSlice(init.arena.allocator());
    if (args.len <= 1) {
        try printUsage(stdout);
        try stdout.flush();
        return;
    }

    const cmd = args[1];
    if (std.mem.eql(u8, cmd, "serve")) {
        try runIpcServer(init, gpa, io, stdout, args[0], args[2..]);
    } else if (std.mem.eql(u8, cmd, "exec")) {
        try runExec(init, gpa, io, stdout, args[2..]);
    } else if (std.mem.eql(u8, cmd, "tui")) {
        try launchTui(stdout);
    } else if (std.mem.eql(u8, cmd, "agent")) {
        try runAgentDemo(gpa, io, stdout);
    } else if (std.mem.eql(u8, cmd, "provider")) {
        try runProviderDemo(gpa, stdout);
    } else if (std.mem.eql(u8, cmd, "session")) {
        try runSessionDemo(gpa, stdout);
    } else if (std.mem.eql(u8, cmd, "scope")) {
        try runScopeDemo(stdout, args[2..]);
    } else if (std.mem.eql(u8, cmd, "version") or std.mem.eql(u8, cmd, "-V") or std.mem.eql(u8, cmd, "--version")) {
        try telekinesis.printBanner(stdout);
    } else if (std.mem.eql(u8, cmd, "help") or std.mem.eql(u8, cmd, "-h") or std.mem.eql(u8, cmd, "--help")) {
        try printUsage(stdout);
    } else if (std.mem.eql(u8, cmd, "net")) {
        try runNetDemo(gpa, io, stdout);
    } else if (std.mem.eql(u8, cmd, "quic")) {
        try runQuicDemo(gpa, io, stdout);
    } else if (std.mem.eql(u8, cmd, "plugin")) {
        try runPluginDemo(gpa, io, stdout);
    } else if (std.mem.eql(u8, cmd, "acp")) {
        try runAcpDemo(gpa, io, stdout);
    } else if (std.mem.eql(u8, cmd, "lsp")) {
        try runLspDemo(gpa, io, stdout);
    } else {
        try stdout.print("Unknown command: {s}\n\n", .{cmd});
        try printUsage(stdout);
    }
    try stdout.flush();
}

fn printUsage(stdout: *std.Io.Writer) !void {
    try telekinesis.printBanner(stdout);
    try stdout.print(
        \\
        \\Usage: telekinesis <command> [args]
        \\
        \\Primary
        \\  serve [socket]     Start harness IPC daemon (rotary)
        \\  tui                Launch crepuscularity-tui (needs serve)
        \\  exec <prompt...>   Non-interactive one-shot (Codex-style)
        \\
        \\Harness
        \\  agent              Agent loop demo
        \\  scope [name]       Show/list work scopes
        \\  provider           List providers
        \\  session            Session tree demo
        \\  plugin             Plugin registry
        \\  acp                ACP host
        \\  lsp                LSP manager
        \\
        \\Network (product)
        \\  net                P2P transport demo
        \\  quic               QUIC demo
        \\
        \\  version · help
        \\
        \\TUI slash (when connected): /model /tools /scope /permissions
        \\  /compact /new /save /load /fork /merge /tree /clear /help
        \\
    , .{});
}

fn launchTui(stdout: *std.Io.Writer) !void {
    // Prefer PATH tk; fallback to cargo run in ui/tui
    var child = std.process.Child.init(&.{"tk"}, std.heap.page_allocator);
    const term = child.spawnAndWait() catch {
        try stdout.print("tk not on PATH. Build: cd ui/tui && cargo build --release\n", .{});
        try stdout.print("Then: export PATH=\"$PWD/ui/tui/target/release:$PATH\" or: cargo run --manifest-path ui/tui/Cargo.toml\n", .{});
        return;
    };
    _ = term;
}

fn runExec(init: std.process.Init, gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, args: []const []const u8) !void {
    _ = init;
    if (args.len == 0) {
        try stdout.print("Usage: telekinesis exec <prompt...>\n", .{});
        return;
    }
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent = telekinesis.Agent.init(arena, io);
    defer agent.deinit();

    var tools = telekinesis.ToolRegistry.init(arena);
    defer tools.deinit();
    telekinesis.tools.setIo(io);
    try telekinesis.tools.registerBuiltins(&tools);
    telekinesis.computer_use.registerTools(&tools) catch {};
    agent.setTools(&tools);
    try agent.setScope(.coding);

    if (try telekinesis.context.loadProjectInstructions(arena, io, ".")) |loaded| {
        const merged = try telekinesis.context.composeSystemPrompt(arena, agent.system_prompt, loaded.content);
        if (merged) |sp| agent.setSystemPrompt(sp);
    }

    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(arena);
    for (args, 0..) |a, i| {
        if (i > 0) try buf.append(arena, ' ');
        try buf.appendSlice(arena, a);
    }
    const prompt = buf.items;

    if (telekinesis.slash.parse(prompt)) |cmd| {
        switch (cmd) {
            .help => try stdout.print("{s}\n", .{telekinesis.slash.helpText()}),
            .model => |q| try stdout.print("model: {s}\n", .{q}),
            .permissions => |m| try stdout.print("permissions: {s}\n", .{m orelse "workspace_write"}),
            .tools => try stdout.print("tools={d}\n", .{tools.count()}),
            .clear, .compact, .session_new => try stdout.print("session op — use serve + tui\n", .{}),
            .unknown => |n| try stdout.print("unknown: /{s}\n", .{n}),
        }
        return;
    }

    try agent.prompt(prompt);
    // Print last assistant message if any
    var i = agent.messages.items.len;
    while (i > 0) {
        i -= 1;
        const m = agent.messages.items[i];
        if (m.role == .assistant) {
            try stdout.print("{s}\n", .{m.content});
            return;
        }
    }
    try stdout.print("(no assistant message)\n", .{});
}

fn runAgentDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent = telekinesis.Agent.init(arena, io);
    defer agent.deinit();

    var tools = telekinesis.ToolRegistry.init(arena);
    defer tools.deinit();
    telekinesis.tools.setIo(io);
    try telekinesis.tools.registerBuiltins(&tools);
    telekinesis.computer_use.registerTools(&tools) catch {};
    agent.setTools(&tools);
    try agent.setScope(.coding);

    try agent.subscribe(null, onEvent);
    try agent.prompt("Hello, telekinesis + rotary");
    try stdout.print("agent ok · tools={d} · scope=coding\n", .{tools.count()});
}

fn onEvent(ctx: ?*anyopaque, event: telekinesis.Event) void {
    _ = ctx;
    std.log.info("event: {}", .{event});
}

fn runScopeDemo(stdout: *std.Io.Writer, args: []const []const u8) !void {
    if (args.len == 0) {
        try stdout.print("scopes: coding research plan ask computer_use\n", .{});
        return;
    }
    if (telekinesis.mode.Scope.fromString(args[0])) |s| {
        const p = telekinesis.mode.profile(s);
        try stdout.print("scope={s}\n{s}\n", .{ s.name(), p.system_addendum });
    } else {
        try stdout.print("unknown scope: {s}\n", .{args[0]});
    }
}

fn runProviderDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var providers = telekinesis.ProviderRegistry.init(gpa);
    defer providers.deinit();
    try providers.add(.openai);
    try providers.add(.anthropic);
    try providers.add(.google);
    try providers.add(.local);
    try stdout.print("providers: {d}\n", .{providers.count()});
}

fn runSessionDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var s = try telekinesis.Session.init(gpa, "demo", "demo");
    defer s.deinit();
    _ = try s.append(.user, "hello");
    _ = try s.append(.assistant, "hi from rotary");
    try stdout.print("session {s} entries={d}\n", .{ s.id, s.entries.items.len });
}

fn runPluginDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var registry = telekinesis.plugin.Registry.init(gpa, io);
    defer registry.deinit();
    try stdout.print("plugin registry ready (pi-compatible)\n", .{});
}

fn runAcpDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var host = telekinesis.acp.Host.init(gpa, io);
    defer host.deinit();
    try stdout.print("ACP host ready\n", .{});
}

fn runLspDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var manager = telekinesis.lsp.Manager.init(gpa, io);
    defer manager.deinit();
    try stdout.print("LSP manager ready\n", .{});
}

fn runNetDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    _ = gpa;
    _ = io;
    try stdout.print("net: QUIC/P2P product layer (see src/net.zig)\n", .{});
}

fn runQuicDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    _ = gpa;
    _ = io;
    try stdout.print("quic: demo via net transport\n", .{});
}

fn runIpcServer(init: std.process.Init, gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, argv0: []const u8, args: []const []const u8) !void {
    _ = argv0;
    const path = if (args.len > 0) args[0] else blk: {
        if (init.environ_map.get("TELEKINESIS_SOCKET")) |p| break :blk p;
        break :blk "/tmp/telekinesis.sock";
    };

    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent_instance = telekinesis.Agent.init(arena, io);
    defer agent_instance.deinit();

    var tool_registry = telekinesis.ToolRegistry.init(arena);
    defer tool_registry.deinit();
    telekinesis.tools.setIo(io);
    try telekinesis.tools.registerBuiltins(&tool_registry);
    telekinesis.computer_use.registerTools(&tool_registry) catch {};
    agent_instance.setTools(&tool_registry);
    try agent_instance.setScope(.coding);

    // Project AGENTS.md / context
    if (try telekinesis.context.loadProjectInstructions(arena, io, ".")) |loaded| {
        const merged = try telekinesis.context.composeSystemPrompt(arena, agent_instance.system_prompt, loaded.content);
        if (merged) |sp| agent_instance.setSystemPrompt(sp);
    }

    var plugin_registry = telekinesis.plugin.Registry.init(gpa, io);
    defer plugin_registry.deinit();

    var acp_host = telekinesis.acp.Host.init(gpa, io);
    defer acp_host.deinit();

    const data_dir = blk: {
        if (init.environ_map.get("TELEKINESIS_HOME")) |h| break :blk h;
        break :blk ".telekinesis";
    };
    var session_store = telekinesis.session.Store.init(gpa, io, data_dir) catch |err| {
        try stdout.print("session store: {}\n", .{err});
        return err;
    };
    defer session_store.deinit();

    var server = telekinesis.ipc.Server.init(gpa, io, path);
    defer server.deinit();
    server.attachAgent(&agent_instance);
    server.attachTools(&tool_registry);
    server.attachPlugins(&plugin_registry);
    server.attachSessionStore(&session_store);
    server.attachAcpHost(&acp_host);

    try stdout.print("telekinesis serve · socket {s}\n", .{path});
    try stdout.print("harness rotary {s} · tools={d} · scope=coding\n", .{ telekinesis.rotary.version, tool_registry.count() });
    try stdout.print("TUI: telekinesis tui   ·   exec: telekinesis exec \"…\"\n", .{});
    try stdout.flush();
    try server.run();
}

pub const std_options: std.Options = .{
    .log_level = .info,
};
