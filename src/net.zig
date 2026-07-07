const std = @import("std");

const log = std.log.scoped(.net);

pub const DeviceId = u128;

pub const DeviceInfo = struct {
    id: DeviceId,
    name: []const u8,
    addresses: []const []const u8,
};

pub const Peer = struct {
    id: DeviceId,
    address: []const u8,
    connected: bool = false,
};

pub const SignalingMessage = struct {
    type: []const u8,
    device_id: ?DeviceId = null,
    device_name: ?[]const u8 = null,
    peer_id: ?DeviceId = null,
    candidates: ?[]const []const u8 = null,
    session_id: ?[]const u8 = null,
};

pub const SignalingClient = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    server_url: []const u8,
    device_id: DeviceId,
    device_name: []const u8,
    registered: bool = false,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, server_url: []const u8, device_id: DeviceId, device_name: []const u8) !SignalingClient {
        return .{
            .allocator = allocator,
            .io = io,
            .server_url = try allocator.dupe(u8, server_url),
            .device_id = device_id,
            .device_name = try allocator.dupe(u8, device_name),
        };
    }

    pub fn deinit(self: *SignalingClient) void {
        self.allocator.free(self.server_url);
        self.allocator.free(self.device_name);
    }

    pub fn announce(self: *SignalingClient) !void {
        log.info("announcing device {x} ({s}) to {s}", .{
            self.device_id,
            self.device_name,
            self.server_url,
        });
        self.registered = true;
    }

    pub fn discoverPeer(self: *SignalingClient, peer_id: DeviceId) !Peer {
        log.info("discovering peer {x}", .{peer_id});
        _ = self;
        return .{
            .id = peer_id,
            .address = "unknown",
        };
    }

    pub fn exchangeCandidates(self: *SignalingClient, peer_id: DeviceId, candidates: []const []const u8) !void {
        log.info("exchanging {d} candidates with peer {x}", .{ candidates.len, peer_id });
        _ = self;
    }
};

pub const Transport = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    local_device_id: DeviceId,
    peers: std.ArrayList(Peer),

    pub fn init(allocator: std.mem.Allocator, io: std.Io, local_device_id: DeviceId) Transport {
        return .{
            .allocator = allocator,
            .io = io,
            .local_device_id = local_device_id,
            .peers = std.ArrayList(Peer).empty,
        };
    }

    pub fn deinit(self: *Transport) void {
        self.peers.deinit(self.allocator);
    }

    pub fn connectPeer(self: *Transport, peer: Peer) !void {
        log.info("connecting to peer {x} at {s}", .{ peer.id, peer.address });
        var p = peer;
        p.connected = true;
        try self.peers.append(self.allocator, p);
    }

    pub fn disconnectPeer(self: *Transport, peer_id: DeviceId) void {
        for (self.peers.items, 0..) |peer, i| {
            if (peer.id == peer_id) {
                _ = self.peers.swapRemove(i);
                log.info("disconnected from peer {x}", .{peer_id});
                return;
            }
        }
    }

    pub fn connectedPeers(self: *const Transport) []const Peer {
        return self.peers.items;
    }
};

pub const Relay = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    endpoint: []const u8,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, endpoint: []const u8) !Relay {
        return .{
            .allocator = allocator,
            .io = io,
            .endpoint = try allocator.dupe(u8, endpoint),
        };
    }

    pub fn deinit(self: *Relay) void {
        self.allocator.free(self.endpoint);
    }

    pub fn relay(self: *Relay, peer_id: DeviceId, data: []const u8) !void {
        log.info("relaying {d} bytes for peer {x} via {s}", .{
            data.len,
            peer_id,
            self.endpoint,
        });
    }
};

pub fn generateDeviceId(io: std.Io) DeviceId {
    var bytes: [16]u8 = undefined;
    io.randomSecure(&bytes) catch {
        @memset(&bytes, 0);
        bytes[0] = 1;
    };
    return std.mem.readInt(u128, &bytes, .little);
}

test "signaling client can announce" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var client = try SignalingClient.init(gpa, io, "https://signal.example.com", 0x1234, "test-device");
    defer client.deinit();
    try client.announce();
    try std.testing.expect(client.registered);
}

test "transport connect and disconnect peer" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var transport = Transport.init(gpa, io, 0xaaaa);
    defer transport.deinit();

    try transport.connectPeer(.{ .id = 0xbbbb, .address = "127.0.0.1:4433" });
    try std.testing.expectEqual(@as(usize, 1), transport.connectedPeers().len);

    transport.disconnectPeer(0xbbbb);
    try std.testing.expectEqual(@as(usize, 0), transport.connectedPeers().len);
}

test "relay stores endpoint" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var relay = try Relay.init(gpa, io, "relay.example.com:4433");
    defer relay.deinit();
    try std.testing.expectEqualStrings("relay.example.com:4433", relay.endpoint);
}

test "generate device id is non-zero" {
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    const id = generateDeviceId(io);
    try std.testing.expect(id != 0);
}
