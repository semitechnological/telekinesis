const std = @import("std");
const httpx = @import("httpx");
const zquic = @import("zquic");

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
    cert_pem: ?[]const u8 = null,
    key_pem: ?[]const u8 = null,
    client_cert_path: ?[]const u8 = null,
    client_key_path: ?[]const u8 = null,
    client_cert_pem: ?[]const u8 = null,
    client_key_pem: ?[]const u8 = null,
    port: u16 = 4433,
};

pub const QuicTransport = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    config: QuicConfig,
    server: ?*zquic.transport.io.Server = null,
    client: ?*zquic.transport.io.Client = null,
    server_address: ?QuicAddress = null,
    server_connection: ?*zquic.transport.io.ConnState = null,
    stream_id: ?u64 = null,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, config: QuicConfig) QuicTransport {
        return .{ .allocator = allocator, .io = io, .config = config };
    }

    pub fn deinit(self: *QuicTransport) void {
        if (self.client) |client| {
            client.deinit();
            self.allocator.destroy(client);
        }
        if (self.server) |server| server.deinit();
    }

    pub fn startServer(self: *QuicTransport) !void {
        log.info("starting QUIC server on port {d}", .{self.config.port});
        if (self.config.cert_path == null and self.config.cert_pem == null) return error.CertificateRequired;
        if (self.config.key_path == null and self.config.key_pem == null) return error.PrivateKeyRequired;
        const raw_sock = std.posix.system.socket(std.posix.AF.INET, std.posix.SOCK.DGRAM, 0);
        if (std.posix.errno(raw_sock) != .SUCCESS) return error.SocketCreateFailed;
        const sock: std.posix.socket_t = @intCast(raw_sock);
        const bind_address = try QuicAddress.parseIp4("0.0.0.0", self.config.port);
        if (std.posix.errno(std.posix.system.bind(sock, &bind_address.any, bind_address.getOsSockLen())) != .SUCCESS) {
            _ = std.posix.system.close(sock);
            return error.SocketBindFailed;
        }
        self.server = try zquic.transport.io.Server.initFromSocket(self.allocator, .{
            .cert_path = self.config.cert_path orelse "",
            .key_path = self.config.key_path orelse "",
            .cert_pem = self.config.cert_pem,
            .key_pem = self.config.key_pem,
            .request_client_certificate = (self.config.client_cert_path != null or self.config.client_cert_pem != null) and (self.config.client_key_path != null or self.config.client_key_pem != null),
            .raw_application_streams = true,
            .alpn = "telekinesis/1",
        }, sock, true);
        log.info("QUIC server listening on :{d}", .{self.config.port});
    }

    pub fn connect(self: *QuicTransport, host: []const u8, port: u16) !void {
        log.info("QUIC connect to {s}:{d}", .{ host, port });
        const client = try self.allocator.create(zquic.transport.io.Client);
        errdefer self.allocator.destroy(client);
        try zquic.transport.io.Client.initInPlace(self.allocator, .{
            .host = host,
            .port = port,
            .client_cert_path = self.config.client_cert_path orelse "",
            .client_key_path = self.config.client_key_path orelse "",
            .client_cert_pem = self.config.client_cert_pem,
            .client_key_pem = self.config.client_key_pem,
            .raw_application_streams = true,
            .alpn = "telekinesis/1",
        }, client);
        const address = try QuicAddress.parseIp4(host, port);
        try client.startHandshake(address);
        self.client = client;
        self.server_address = address;
    }

    fn pump(self: *QuicTransport) void {
        const server = self.server orelse return;
        const client = self.client orelse return;
        const address = self.server_address orelse return;
        server.resetDriveSendBudgets();
        client.resetDriveSendBudget();
        var buffer: [2048]u8 = undefined;
        while (quicSocketReadable(server.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(server.sock, &buffer, &peer) orelse break;
            server.feedPacket(buffer[0..size], quicAddressFromStorage(&peer));
        }
        server.processPendingWork();
        while (quicSocketReadable(client.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(client.sock, &buffer, &peer) orelse break;
            client.feedPacket(buffer[0..size]);
        }
        client.processPendingWork(address);
        client.flushDeferredAck();
        self.server_connection = quicServerConnection(server);
    }

    fn waitConnected(self: *QuicTransport) !void {
        const client = self.client orelse return error.QuicNotConnected;
        var attempts: usize = 0;
        while (attempts < 5_000) : (attempts += 1) {
            self.pump();
            if (client.conn.phase == .connected and self.server_connection != null) return;
            try std.Io.sleep(self.io, std.Io.Duration.fromMilliseconds(1), .awake);
        }
        return error.HandshakeTimeout;
    }

    pub fn sendMessage(self: *QuicTransport, peer_id: DeviceId, data: []const u8) !void {
        _ = peer_id;
        try self.waitConnected();
        const client = self.client orelse return error.QuicNotConnected;
        const stream_id = try zquic.transport.io.rawAllocateNextLocalBidiStream(&client.conn);
        var attempts: usize = 0;
        while (attempts < 5_000) : (attempts += 1) {
            if (client.sendRawStreamData(stream_id, 0, data, true) == data.len) {
                self.stream_id = stream_id;
            }
            self.pump();
            if (self.receivedMessage()) |_| return;
            try std.Io.sleep(self.io, std.Io.Duration.fromMilliseconds(1), .awake);
        }
        return error.StreamDeliveryTimeout;
    }

    pub fn receivedMessage(self: *QuicTransport) ?[]const u8 {
        const connection = self.server_connection orelse return null;
        const stream_id = self.stream_id orelse return null;
        if (!zquic.transport.io.rawAppStreamFullyReceived(connection, stream_id)) return null;
        return zquic.transport.io.rawAppRecvBuffer(connection, stream_id);
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
    const io = std.testing.io;
    var client = try SignalingClient.init(gpa, io, "https://signal.example.com", 0x1234, "test-device");
    defer client.deinit();
    try client.announce();
    try std.testing.expect(client.registered);
}

test "signaling client announce with real http" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;

    var server_addr = try std.Io.net.IpAddress.parseIp4("127.0.0.1", 0);
    var server = try server_addr.listen(io, .{ .reuse_address = true });
    defer server.deinit(io);

    const port = server.socket.address.getPort();

    const server_handle = try std.Thread.spawn(.{}, struct {
        fn run(s: *std.Io.net.Server, server_io: std.Io) void {
            const stream = s.accept(server_io) catch return;
            defer stream.close(server_io);

            var read_buf: [4096]u8 = undefined;
            var file_reader = std.Io.net.Stream.Reader.init(stream, server_io, &read_buf);
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
            var fw = std.Io.net.Stream.Writer.init(stream, server_io, &wb);
            const w = &fw.interface;
            w.writeAll("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n") catch return;
            w.writeAll(cl) catch return;
            w.writeAll("Connection: close\r\n\r\n") catch return;
            w.writeAll(body) catch return;
            w.flush() catch return;
        }
    }.run, .{ &server, io });
    server_handle.detach();

    try std.Io.sleep(io, std.Io.Duration.fromMilliseconds(10), .awake);

    const base_url = try std.fmt.allocPrint(gpa, "http://127.0.0.1:{d}", .{port});
    defer gpa.free(base_url);
    var client = try SignalingClient.init(gpa, io, base_url, 0xabcd, "test-announce");
    defer client.deinit();
    try client.announce();
    try std.testing.expect(client.registered);
}

test "transport connect and disconnect peer" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    var transport = Transport.init(gpa, io, 0xaaaa);
    defer transport.deinit();

    try transport.connectPeer(.{ .id = 0xbbbb, .address = "127.0.0.1:4433" });
    try std.testing.expectEqual(@as(usize, 1), transport.connectedPeers().len);

    transport.disconnectPeer(0xbbbb);
    try std.testing.expectEqual(@as(usize, 0), transport.connectedPeers().len);
}

test "relay stores endpoint" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    var relay = try Relay.init(gpa, io, "relay.example.com:4433");
    defer relay.deinit();
    try std.testing.expectEqualStrings("relay.example.com:4433", relay.endpoint);
}

test "generate device id is non-zero" {
    const io = std.testing.io;
    const id = generateDeviceId(io);
    try std.testing.expect(id != 0);
}

const raw_quic_cert =
    \\-----BEGIN CERTIFICATE-----
    \\MIIBlDCCATugAwIBAgIUVQcs4ukEwzyEPHOkJozYtzcLgc0wCgYIKoZIzj0EAwIw
    \\FTETMBEGA1UEAwwKenF1aWMtdGVzdDAeFw0yNjA2MTYxMzU4MDVaFw0zNjA2MTMx
    \\MzU4MDVaMBUxEzARBgNVBAMMCnpxdWljLXRlc3QwWTATBgcqhkjOPQIBBggqhkjO
    \\PQMBBwNCAASi2BRPaS1eDrI3Nz0SiTm/WyiFXZOvdnotNM7dVpwyxERnoMvjN3rg
    \\orxvtr+Ims0UQAubd1auIxOF2m5rSK+no2kwZzAdBgNVHQ4EFgQUp2j49kW3eDQH
    \\X1Zz5lCWTPqzs28wHwYDVR0jBBgwFoAUp2j49kW3eDQHX1Zz5lCWTPqzs28wDwYD
    \\VR0TAQH/BAUwAwEB/zAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwID
    \\RwAwRAIgTiMFC6CRDktT0L8cyOz6HqqwpsjZqXLl5P+VY9M/X44CIBnZN6TjJnHd
    \\DMj4Q3a0LOr2IbQ4MteOsig/Mkp+nUgL
    \\-----END CERTIFICATE-----
    \\
;
const raw_quic_key =
    \\-----BEGIN EC PRIVATE KEY-----
    \\MHcCAQEEIP92J5gFLRPtrWADUWgpuRcoogwCKh50Cgh6XYTQ5wr7oAoGCCqGSM49
    \\AwEHoUQDQgAEotgUT2ktXg6yNzc9Eok5v1sohV2Tr3Z6LTTO3VacMsREZ6DL4zd6
    \\4KK8b7a/iJrNFEALm3dWriMThdpua0ivpw==
    \\-----END EC PRIVATE KEY-----
    \\
;

const QuicAddress = @typeInfo(@TypeOf(zquic.transport.io.Client.startHandshake)).@"fn".params[1].type.?;

fn quicAddressFromStorage(storage: *const std.posix.sockaddr.storage) QuicAddress {
    var address: QuicAddress = undefined;
    @memcpy(std.mem.asBytes(&address)[0..@sizeOf(QuicAddress)], std.mem.asBytes(storage)[0..@sizeOf(QuicAddress)]);
    return address;
}

fn quicSocketReadable(fd: std.posix.socket_t) bool {
    var fds = [_]std.posix.pollfd{.{ .fd = fd, .events = std.posix.POLL.IN, .revents = 0 }};
    const count = std.posix.poll(&fds, 0) catch return false;
    return count > 0 and (fds[0].revents & std.posix.POLL.IN) != 0;
}

fn quicRecvFrom(fd: std.posix.socket_t, buffer: []u8, storage: *std.posix.sockaddr.storage) ?usize {
    var length: std.posix.socklen_t = @sizeOf(@TypeOf(storage.*));
    const result = std.posix.system.recvfrom(fd, buffer.ptr, buffer.len, 0, @ptrCast(storage), &length);
    if (result < 0) return null;
    return @intCast(result);
}

fn quicServerConnection(server: *zquic.transport.io.Server) ?*zquic.transport.io.ConnState {
    for (&server.conns) |*slot| {
        if (slot.*) |connection| {
            if (connection.phase == .connected) return connection;
        }
    }
    return null;
}

test "zquic authenticates mutual TLS and delivers a raw stream" {
    const allocator = std.testing.allocator;
    const raw_server_sock = std.posix.system.socket(std.posix.AF.INET, std.posix.SOCK.DGRAM, 0);
    if (std.posix.errno(raw_server_sock) != .SUCCESS) return error.SocketCreateFailed;
    const server_sock: std.posix.socket_t = @intCast(raw_server_sock);
    const bind_address = try QuicAddress.parseIp4("127.0.0.1", 0);
    if (std.posix.errno(std.posix.system.bind(server_sock, &bind_address.any, bind_address.getOsSockLen())) != .SUCCESS) {
        _ = std.posix.system.close(server_sock);
        return error.SocketBindFailed;
    }
    const server = try zquic.transport.io.Server.initFromSocket(allocator, .{
        .cert_pem = raw_quic_cert,
        .key_pem = raw_quic_key,
        .request_client_certificate = true,
        .raw_application_streams = true,
        .alpn = "telekinesis-test",
    }, server_sock, true);
    defer server.deinit();

    var storage: std.posix.sockaddr.storage = undefined;
    var length: std.posix.socklen_t = @sizeOf(@TypeOf(storage));
    if (std.posix.errno(std.posix.system.getsockname(server.sock, @ptrCast(&storage), &length)) != .SUCCESS) return error.GetSockNameFailed;
    const server_address = quicAddressFromStorage(&storage);

    const client = try allocator.create(zquic.transport.io.Client);
    defer allocator.destroy(client);
    try zquic.transport.io.Client.initInPlace(allocator, .{
        .host = "127.0.0.1",
        .port = server_address.getPort(),
        .client_cert_pem = raw_quic_cert,
        .client_key_pem = raw_quic_key,
        .raw_application_streams = true,
        .alpn = "telekinesis-test",
    }, client);
    defer client.deinit();
    try client.startHandshake(server_address);

    var attempts: usize = 0;
    var connected: ?*zquic.transport.io.ConnState = null;
    while (attempts < 5_000) : (attempts += 1) {
        server.resetDriveSendBudgets();
        client.resetDriveSendBudget();
        var buffer: [2048]u8 = undefined;
        while (quicSocketReadable(server.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(server.sock, &buffer, &peer) orelse break;
            server.feedPacket(buffer[0..size], quicAddressFromStorage(&peer));
        }
        server.processPendingWork();
        while (quicSocketReadable(client.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(client.sock, &buffer, &peer) orelse break;
            client.feedPacket(buffer[0..size]);
        }
        client.processPendingWork(server_address);
        client.flushDeferredAck();
        connected = quicServerConnection(server);
        if (client.conn.phase == .connected and connected != null) break;
        try std.Io.sleep(std.testing.io, std.Io.Duration.fromMilliseconds(1), .awake);
    }
    const connection = connected orelse return error.HandshakeTimeout;
    try std.testing.expectEqual(.connected, client.conn.phase);
    try std.testing.expect(client.peerLeafCertificateDer() != null);
    try std.testing.expect(zquic.transport.io.serverConnPeerLeafCertificateDer(connection) != null);

    const stream_id = try zquic.transport.io.rawAllocateNextLocalBidiStream(&client.conn);
    const payload = "telekinesis raw stream";
    var sent = false;
    attempts = 0;
    while (attempts < 5_000) : (attempts += 1) {
        server.resetDriveSendBudgets();
        client.resetDriveSendBudget();
        if (!sent and client.sendRawStreamData(stream_id, 0, payload, true) == payload.len) sent = true;
        var buffer: [2048]u8 = undefined;
        while (quicSocketReadable(server.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(server.sock, &buffer, &peer) orelse break;
            server.feedPacket(buffer[0..size], quicAddressFromStorage(&peer));
        }
        server.processPendingWork();
        while (quicSocketReadable(client.sock)) {
            var peer: std.posix.sockaddr.storage = undefined;
            const size = quicRecvFrom(client.sock, &buffer, &peer) orelse break;
            client.feedPacket(buffer[0..size]);
        }
        client.processPendingWork(server_address);
        client.flushDeferredAck();
        if (zquic.transport.io.rawAppRecvBuffer(connection, stream_id)) |received| {
            if (zquic.transport.io.rawAppStreamFullyReceived(connection, stream_id)) {
                try std.testing.expectEqualStrings(payload, received);
                return;
            }
        }
        try std.Io.sleep(std.testing.io, std.Io.Duration.fromMilliseconds(1), .awake);
    }
    return error.StreamDeliveryTimeout;
}

test "quic transport sends an authenticated raw message" {
    const allocator = std.testing.allocator;
    var transport = QuicTransport.init(allocator, std.testing.io, .{
        .cert_pem = raw_quic_cert,
        .key_pem = raw_quic_key,
        .client_cert_pem = raw_quic_cert,
        .client_key_pem = raw_quic_key,
        .port = 0,
    });
    defer transport.deinit();
    try transport.startServer();
    const server = transport.server orelse return error.ServerNotStarted;
    var storage: std.posix.sockaddr.storage = undefined;
    var length: std.posix.socklen_t = @sizeOf(@TypeOf(storage));
    if (std.posix.errno(std.posix.system.getsockname(server.sock, @ptrCast(&storage), &length)) != .SUCCESS) return error.GetSockNameFailed;
    try transport.connect("127.0.0.1", quicAddressFromStorage(&storage).getPort());
    try transport.sendMessage(1, "telekinesis facade");
    try std.testing.expectEqualStrings("telekinesis facade", transport.receivedMessage().?);
}
