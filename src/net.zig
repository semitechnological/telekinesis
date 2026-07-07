const std = @import("std");

const log = std.log.scoped(.net);

pub const DeviceId = u128;

pub const Peer = struct {
    id: DeviceId,
    address: []const u8,
};

pub const SignalingClient = struct {
    allocator: std.mem.Allocator,
    io: std.Io,

    pub fn init(allocator: std.mem.Allocator, io: std.Io) SignalingClient {
        return .{
            .allocator = allocator,
            .io = io,
        };
    }

    pub fn announce(self: *SignalingClient) !void {
        _ = self;
        log.info("announcing device to signaling server", .{});
    }

    pub fn connect(self: *SignalingClient, peer: Peer) !void {
        log.info("connecting to peer: {s}", .{peer.address});
        _ = self;
    }
};

test "signaling client can announce" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var client = SignalingClient.init(gpa, io);
    try client.announce();
}
