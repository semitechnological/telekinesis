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
        } else {
            try stdout.print("Unknown command: {s}\n", .{cmd});
        }
    } else {
        try stdout.print("Usage: telekinesis <agent|provider>\n", .{});
    }

    try stdout.flush();
}

fn runAgentDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent = telekinesis.Agent.init(arena, io);
    defer agent.deinit();

    agent.subscribe(null, onEvent);
    try agent.prompt("Hello, Telekinesis!");

    try stdout.print("Agent demo complete.\n", .{});
}

fn onEvent(ctx: ?*anyopaque, event: telekinesis.Event) void {
    _ = ctx;
    std.log.info("event: {}", .{event});
}

fn runProviderDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var providers = telekinesis.provider.Registry.init(gpa);
    defer providers.deinit();

    try providers.add(.openai);
    try providers.add(.anthropic);

    try stdout.print("Providers: {d}\n", .{providers.count()});
}

pub const std_options: std.Options = .{
    .log_level = .info,
};
