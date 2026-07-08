//! Thread Pool and Async Task Offloading HTTP Server Example
//!
//! Demonstrates:
//! 1. Starting a server with a configured thread pool (worker pool).
//! 2. Handling requests concurrently on the worker threads.

const std = @import("std");
const httpx = @import("httpx");

fn helloHandler(ctx: *httpx.Context) anyerror!httpx.Response {
    return ctx.text("Hello from the worker pool thread!");
}

fn asyncTaskHandler(ctx: *httpx.Context) anyerror!httpx.Response {
    const io = std.Io.Threaded.global_single_threaded.io();
    const start = std.Io.Timestamp.now(io, .awake).toMilliseconds();
    // Simulate a slow blocking computation/I/O task.
    // Since the server configures threads > 0, this request runs on a background worker thread.
    // It does not block other connections from being accepted or processed by other threads.
    httpx.sleepMs(50);
    const elapsed = std.Io.Timestamp.now(io, .awake).toMilliseconds() - start;

    const msg = try std.fmt.allocPrint(ctx.allocator, "Processed blocking task in {d}ms on worker thread pool!\n", .{elapsed});
    defer ctx.allocator.free(msg);

    return ctx.text(msg);
}

pub fn main() !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    std.debug.print("=== Thread Pool HTTP Server Example ===\n\n", .{});

    // Configure server with 4 worker threads
    var server = httpx.Server.initWithConfig(allocator, .{
        .host = "127.0.0.1",
        .port = 8080,
        .port_conflict = .increment,
        .threads = 4, // Enables the Executor thread pool
        .keep_alive = true,
    });
    defer server.deinit();

    try server.get("/", helloHandler);
    try server.get("/async", asyncTaskHandler);

    std.debug.print("Server Configuration:\n", .{});
    std.debug.print("  Host: {s}\n", .{server.config.host});
    std.debug.print("  Port: {d}\n", .{server.config.port});
    std.debug.print("  Worker Threads: {d}\n", .{server.config.threads});
    std.debug.print("  ThreadPool enabled: {}\n", .{server.executor != null});

    std.debug.print("\nRegistered routes:\n", .{});
    std.debug.print("  GET  /        -> helloHandler\n", .{});
    std.debug.print("  GET  /async   -> asyncTaskHandler (simulates slow workload)\n", .{});

    std.debug.print("\nServer starting... Try: http://127.0.0.1:{d}/async\n", .{server.config.port});

    try server.listen();
}
