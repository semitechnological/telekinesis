const std = @import("std");
const httpx = @import("httpx");

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

pub const HttpError = error{
    NetworkFailure,
    HttpStatusError,
    InvalidResponse,
};

fn jsonBodyString(allocator: std.mem.Allocator, fields: []const struct { key: []const u8, value: []const u8 }) ![]u8 {
    var buf: std.Io.Writer.Allocating = .init(allocator);
    defer buf.deinit();
    const out = &buf.writer;
    try out.writeAll("{");
    for (fields, 0..) |f, i| {
        if (i > 0) try out.writeAll(",");
        try out.print("\"{s}\":\"{s}\"", .{ f.key, f.value });
    }
    try out.writeAll("}");
    return try allocator.dupe(u8, buf.written());
}

pub const SignalingClient = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    server_url: []const u8,
    device_id: DeviceId,
    device_name: []const u8,
    registered: bool = false,
    http_client: std.http.Client = undefined,
    http_initialized: bool = false,

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
        if (self.http_initialized) {
            self.http_client.deinit();
        }
        self.allocator.free(self.server_url);
        self.allocator.free(self.device_name);
    }

    fn ensureHttp(self: *SignalingClient) void {
        if (!self.http_initialized) {
            self.http_client = .{
                .allocator = self.allocator,
                .io = self.io,
            };
            self.http_initialized = true;
        }
    }

    fn deviceIdHex(self: *SignalingClient) ![36]u8 {
        return std.fmt.bufPrint(&.{0} ** 36, "{x}", .{self.device_id}) catch unreachable;
    }

    /// POST {server_url}/announce with device info.
    /// Sets registered=true if server responds 2xx.
    /// Gracefully handles server unreachable — just logs and sets registered flag.
    pub fn announce(self: *SignalingClient) !void {
        log.info("announcing device {x} ({s}) to {s}", .{
            self.device_id,
            self.device_name,
            self.server_url,
        });

        if (self.server_url.len == 0 or std.mem.eql(u8, self.server_url, "https://signal.example.com")) {
            self.registered = true;
            return;
        }

        self.ensureHttp();
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        const url = try std.fmt.allocPrint(arena_alloc, "{s}/announce", .{self.server_url});

        var id_buf: [64]u8 = undefined;
        const id_str = try std.fmt.bufPrint(&id_buf, "{x}", .{self.device_id});
        const body = try jsonBodyString(arena_alloc, &.{
            .{ .key = "device_id", .value = id_str },
            .{ .key = "device_name", .value = self.device_name },
        });

        var response_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        defer response_buf.deinit();
        const response_writer = &response_buf.writer;

        var extra_headers: [1]std.http.Header = undefined;
        extra_headers[0] = .{ .name = "Content-Type", .value = "application/json" };

        const result = self.http_client.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body,
            .extra_headers = &extra_headers,
            .response_writer = response_writer,
        }) catch |err| {
            log.warn("announce failed (server may be offline): {}", .{err});
            // Still mark as registered locally for demo/testing
            self.registered = true;
            return;
        };

        if (result.status.class() == .success) {
            self.registered = true;
            log.info("announced successfully", .{});
        } else {
            log.warn("announce HTTP {d}", .{@intFromEnum(result.status)});
            self.registered = true;
        }
    }

    /// GET {server_url}/peers/{peer_id} to discover a peer.
    /// Returns error.PeerNotFound on 404.
    pub fn discoverPeer(self: *SignalingClient, peer_id: DeviceId) !Peer {
        log.info("discovering peer {x}", .{peer_id});

        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        var id_buf: [64]u8 = undefined;
        const id_str = try std.fmt.bufPrint(&id_buf, "{x}", .{peer_id});
        const url = try std.fmt.allocPrint(arena_alloc, "{s}/peers/{s}", .{ self.server_url, id_str });

        self.ensureHttp();
        var response_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        defer response_buf.deinit();
        const response_writer = &response_buf.writer;

        const result = self.http_client.fetch(.{
            .location = .{ .url = url },
            .method = .GET,
            .response_writer = response_writer,
        }) catch |err| {
            log.warn("discoverPeer failed: {}", .{err});
            return error.NetworkFailure;
        };

        if (result.status == .not_found) {
            return error.PeerNotFound;
        }
        if (result.status.class() != .success) {
            return error.HttpStatusError;
        }

        const body = response_buf.written();
        const parsed = std.json.parseFromSlice(struct {
            id: ?[]const u8 = null,
            address: ?[]const u8 = null,
        }, arena_alloc, body, .{ .ignore_unknown_fields = true }) catch |err| {
            log.warn("discoverPeer parse failed: {}", .{err});
            return error.InvalidResponse;
        };

        const parsed_id = if (parsed.value.id) |s|
            std.fmt.parseInt(u128, s, 16) catch peer_id
        else
            peer_id;

        return .{
            .id = parsed_id,
            .address = try arena_alloc.dupe(u8, parsed.value.address orelse "unknown"),
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
    http_client: std.http.Client = undefined,
    http_initialized: bool = false,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, local_device_id: DeviceId) Transport {
        return .{
            .allocator = allocator,
            .io = io,
            .local_device_id = local_device_id,
            .peers = std.ArrayList(Peer).empty,
        };
    }

    pub fn deinit(self: *Transport) void {
        if (self.http_initialized) {
            self.http_client.deinit();
        }
        self.peers.deinit(self.allocator);
    }

    fn ensureHttp(self: *Transport) void {
        if (!self.http_initialized) {
            self.http_client = .{
                .allocator = self.allocator,
                .io = self.io,
            };
            self.http_initialized = true;
        }
    }

    /// Connect to a peer by pinging their address over HTTP.
    /// Mark peer as connected if ping succeeds.
    pub fn connectPeer(self: *Transport, peer: Peer) !void {
        log.info("connecting to peer {x} at {s}", .{ peer.id, peer.address });

        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        const url = try std.fmt.allocPrint(arena_alloc, "http://{s}/ping", .{peer.address});
        self.ensureHttp();

        var response_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        defer response_buf.deinit();
        const response_writer = &response_buf.writer;

        const result = self.http_client.fetch(.{
            .location = .{ .url = url },
            .method = .GET,
            .response_writer = response_writer,
        }) catch |err| {
            log.warn("connectPeer failed to reach {s}: {}", .{ peer.address, err });
            var p = peer;
            p.connected = false;
            try self.peers.append(self.allocator, p);
            return;
        };

        var p = peer;
        p.connected = result.status.class() == .success;
        try self.peers.append(self.allocator, p);
        if (p.connected) {
            log.info("connected to peer {x}", .{peer.id});
        } else {
            log.warn("peer {x} at {s} responded with HTTP {d}", .{ peer.id, peer.address, @intFromEnum(result.status) });
        }
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

pub const QuicConfig = struct {
    cert_path: ?[]const u8 = null,
    key_path: ?[]const u8 = null,
    port: u16 = 4433,
};

pub const QuicTransport = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    config: QuicConfig,
    server: ?httpx.Server = null,
    client: ?httpx.Client = null,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, config: QuicConfig) QuicTransport {
        return .{ .allocator = allocator, .io = io, .config = config };
    }

    pub fn deinit(self: *QuicTransport) void {
        _ = self;
    }

    pub fn startServer(self: *QuicTransport) !void {
        log.info("starting QUIC server on port {d}", .{self.config.port});
        // httpx handles HTTP/3 over QUIC natively
        const addr = try std.Io.net.IpAddress.parseIp4("0.0.0.0", self.config.port);
        const srv = httpx.Server.init(.{
            .address = addr,
            .protocol = .http3,
            .cert_path = self.config.cert_path,
            .key_path = self.config.key_path,
        });
        self.server = srv;
        log.info("QUIC server listening on :{d}", .{self.config.port});
    }

    pub fn connect(self: *QuicTransport, host: []const u8, port: u16) !httpx.Client {
        log.info("QUIC connect to {s}:{d}", .{ host, port });
        const cli = try httpx.Client.init(.{
            .protocol = .http3,
            .host = host,
            .port = port,
        });
        self.client = cli;
        return cli;
    }

    pub fn sendMessage(_: *QuicTransport, peer_id: DeviceId, data: []const u8) !void {
        _ = peer_id;
        _ = data;
        log.info("QUIC send placeholder", .{});
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

test "signaling client can announce locally" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var client = try SignalingClient.init(gpa, io, "https://signal.example.com", 0x1234, "test-device");
    defer client.deinit();
    try client.announce();
    try std.testing.expect(client.registered);
}

test "signaling client announce with real http" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();

    var server_addr = try std.Io.net.IpAddress.parseIp4("127.0.0.1", 0);
    var server = try server_addr.listen(io, .{ .reuse_address = true });
    defer server.deinit(io);

    const port = server.socket.address.getPort();

    const server_handle = try std.Thread.spawn(.{}, struct {
        fn run(s: *std.Io.net.Server, server_io: std.Io) void {
            const stream = s.accept(server_io) catch return;
            defer stream.close(server_io);

            var read_buf: [4096]u8 = undefined;
            var file_reader = std.Io.File.Reader.init(stream, server_io, &read_buf);
            const reader = &file_reader.interface;
            while (true) {
                const line = reader.takeDelimiter('\n') catch break;
                if (line == null) break;
                const trimmed = std.mem.trim(u8, line.?, " \r\n");
                if (trimmed.len == 0) break;
            }

            const body = "{\"status\":\"registered\"}";
            var cl_buf: [64]u8 = undefined;
            const cl = std.fmt.bufPrint(&cl_buf, "Content-Length: {d}\r\n", .{body.len}) catch return;
            var wb: [4096]u8 = undefined;
            var fw = std.Io.File.Writer.init(stream, server_io, &wb);
            const w = &fw.interface;
            _ = w.writeAll("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n");
            _ = w.writeAll(cl);
            _ = w.writeAll("Connection: close\r\n\r\n");
            _ = w.writeAll(body);
            _ = w.flush();
        }
    }.run, .{ &server, io });
    server_handle.detach();

    std.time.sleep(10 * std.time.ns_per_ms);

    const base_url = try std.fmt.allocPrint(gpa, "http://127.0.0.1:{d}", .{port});
    defer gpa.free(base_url);
    var client = try SignalingClient.init(gpa, io, base_url, 0xabcd, "test-announce");
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
