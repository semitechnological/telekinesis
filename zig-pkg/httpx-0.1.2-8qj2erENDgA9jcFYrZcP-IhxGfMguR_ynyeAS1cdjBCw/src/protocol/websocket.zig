//! WebSocket Protocol Implementation for httpx.zig
//!
//! Implements RFC 6455: The WebSocket Protocol
//!
//! WebSocket provides full-duplex communication over a single TCP connection.
//! The protocol begins with an HTTP/1.1 upgrade handshake, then switches to a
//! binary framing protocol for bidirectional messaging.
//!
//! ## Clean flat API — no double-namespace
//!
//! ```zig
//! const httpx = @import("httpx");
//!
//! // Server upgrade check
//! if (httpx.isWebSocketUpgrade(&request)) {
//!     const key = httpx.wsExtractKey(&request).?;
//!     const accept = try httpx.wsAcceptKey(key, allocator);
//!     defer allocator.free(accept);
//! }
//!
//! // Encode a text frame (server → client, no mask)
//! const frame = try httpx.wsEncodeFrame(allocator, .text, "hello", true, false, .{0,0,0,0});
//! defer allocator.free(frame);
//!
//! // Decode a received frame
//! const result = try httpx.wsDecodeFrame(allocator, raw_bytes);
//! var f = result.frame;
//! defer f.deinit();
//! ```

const std = @import("std");
const mem = std.mem;
const Allocator = mem.Allocator;
const Request = @import("../core/request.zig").Request;

// Constants

/// WebSocket magic GUID from RFC 6455 §1.3.
pub const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// Types

/// WebSocket frame opcode.
pub const WsOpcode = enum(u4) {
    continuation = 0x0,
    text = 0x1,
    binary = 0x2,
    close = 0x8,
    ping = 0x9,
    pong = 0xA,
    _,

    /// Returns true for control opcodes (close, ping, pong).
    pub fn isControl(self: WsOpcode) bool {
        return @intFromEnum(self) >= 0x8;
    }

    /// Returns true for data opcodes (text, binary, continuation).
    pub fn isData(self: WsOpcode) bool {
        return @intFromEnum(self) < 0x8;
    }
};

/// A decoded WebSocket frame. Call `deinit()` when done.
pub const WsFrame = struct {
    /// Whether this is the final fragment of a message.
    fin: bool,
    opcode: WsOpcode,
    /// Whether the payload was masked (required for client→server).
    masked: bool,
    /// Decoded (unmasked) payload bytes.
    payload: []u8,
    allocator: Allocator,

    pub fn deinit(self: *WsFrame) void {
        self.allocator.free(self.payload);
    }
};

/// WebSocket close status codes per RFC 6455 §7.4.
pub const WsCloseCode = enum(u16) {
    normal = 1000,
    going_away = 1001,
    protocol_error = 1002,
    unsupported_data = 1003,
    no_status = 1005,
    abnormal = 1006,
    invalid_payload = 1007,
    policy_violation = 1008,
    message_too_big = 1009,
    missing_extension = 1010,
    internal_error = 1011,
    _,
};

/// Result of `wsDecodeFrame`.
pub const WsDecodeResult = struct {
    frame: WsFrame,
    /// Number of bytes consumed from the input slice.
    consumed: usize,
};

// Handshake helpers

/// Returns true when `req` is a valid WebSocket upgrade request.
///
/// Checks `Upgrade: websocket`, `Connection: Upgrade`, and `Sec-WebSocket-Key`.
pub fn isWebSocketUpgrade(req: *const Request) bool {
    const upgrade = req.headers.get("Upgrade") orelse return false;
    const connection = req.headers.get("Connection") orelse return false;
    const key = req.headers.get("Sec-WebSocket-Key") orelse return false;
    return std.ascii.eqlIgnoreCase(upgrade, "websocket") and
        std.ascii.indexOfIgnoreCase(connection, "upgrade") != null and
        key.len > 0;
}

/// Returns the `Sec-WebSocket-Key` value, or null if absent.
pub fn wsExtractKey(req: *const Request) ?[]const u8 {
    return req.headers.get("Sec-WebSocket-Key");
}

/// Computes `Sec-WebSocket-Accept` from the client's key (RFC 6455 §1.3).
///
/// Caller owns the returned slice; free with the same allocator.
pub fn wsAcceptKey(client_key: []const u8, allocator: Allocator) ![]u8 {
    const concat = try std.fmt.allocPrint(allocator, "{s}{s}", .{ client_key, WS_GUID });
    defer allocator.free(concat);

    var sha1 = std.crypto.hash.Sha1.init(.{});
    sha1.update(concat);
    var digest: [20]u8 = undefined;
    sha1.final(&digest);

    const encoded_len = std.base64.standard.Encoder.calcSize(digest.len);
    const result = try allocator.alloc(u8, encoded_len);
    _ = std.base64.standard.Encoder.encode(result, &digest);
    return result;
}

/// Builds a `101 Switching Protocols` response header map for a WebSocket upgrade.
///
/// Returns the computed `Sec-WebSocket-Accept` value; caller must free it.
/// Set these on your 101 response:
///   `Upgrade: websocket`
///   `Connection: Upgrade`
///   `Sec-WebSocket-Accept: <returned value>`
pub fn wsUpgradeHeaders(client_key: []const u8, allocator: Allocator) ![]u8 {
    return wsAcceptKey(client_key, allocator);
}

// Frame encoding

/// Encodes a WebSocket frame.
///
/// - `opcode`: frame type (text, binary, ping, pong, close, continuation)
/// - `payload`: raw bytes to send
/// - `fin`: true for the final (or only) fragment
/// - `masked`: true for client→server frames (RFC 6455 requires masking)
/// - `mask_key`: 4-byte key; only used when `masked` is true
///
/// Caller owns the returned slice.
pub fn wsEncodeFrame(
    allocator: Allocator,
    opcode: WsOpcode,
    payload: []const u8,
    fin: bool,
    masked: bool,
    mask_key: [4]u8,
) ![]u8 {
    const ext_len_bytes: usize =
        if (payload.len < 126) 0 else if (payload.len < 65536) 2 else 8;
    const mask_bytes: usize = if (masked) 4 else 0;
    const header_size: usize = 2 + ext_len_bytes + mask_bytes;

    const frame = try allocator.alloc(u8, header_size + payload.len);
    errdefer allocator.free(frame);

    frame[0] = (@as(u8, if (fin) 0x80 else 0x00)) | @as(u8, @intFromEnum(opcode));

    var offset: usize = 2;
    if (payload.len < 126) {
        frame[1] = @as(u8, @intCast(payload.len)) | (if (masked) @as(u8, 0x80) else 0);
    } else if (payload.len < 65536) {
        frame[1] = 126 | (if (masked) @as(u8, 0x80) else 0);
        frame[2] = @intCast((payload.len >> 8) & 0xFF);
        frame[3] = @intCast(payload.len & 0xFF);
        offset = 4;
    } else {
        frame[1] = 127 | (if (masked) @as(u8, 0x80) else 0);
        const l: u64 = payload.len;
        frame[2] = @intCast((l >> 56) & 0xFF);
        frame[3] = @intCast((l >> 48) & 0xFF);
        frame[4] = @intCast((l >> 40) & 0xFF);
        frame[5] = @intCast((l >> 32) & 0xFF);
        frame[6] = @intCast((l >> 24) & 0xFF);
        frame[7] = @intCast((l >> 16) & 0xFF);
        frame[8] = @intCast((l >> 8) & 0xFF);
        frame[9] = @intCast(l & 0xFF);
        offset = 10;
    }

    if (masked) {
        frame[offset..][0..4].* = mask_key;
        offset += 4;
        for (payload, 0..) |b, i| {
            frame[offset + i] = b ^ mask_key[i & 3];
        }
    } else {
        @memcpy(frame[offset..][0..payload.len], payload);
    }

    return frame;
}

/// Encodes a text frame for sending from a server (unmasked, fin=true).
/// Convenience wrapper around `wsEncodeFrame`.
pub fn wsTextFrame(allocator: Allocator, text: []const u8) ![]u8 {
    return wsEncodeFrame(allocator, .text, text, true, false, .{ 0, 0, 0, 0 });
}

/// Encodes a binary frame for sending from a server (unmasked, fin=true).
pub fn wsBinaryFrame(allocator: Allocator, data: []const u8) ![]u8 {
    return wsEncodeFrame(allocator, .binary, data, true, false, .{ 0, 0, 0, 0 });
}

/// Encodes a ping frame.
pub fn wsPingFrame(allocator: Allocator, data: []const u8) ![]u8 {
    return wsEncodeFrame(allocator, .ping, data, true, false, .{ 0, 0, 0, 0 });
}

/// Encodes a pong frame.
pub fn wsPongFrame(allocator: Allocator, data: []const u8) ![]u8 {
    return wsEncodeFrame(allocator, .pong, data, true, false, .{ 0, 0, 0, 0 });
}

/// Encodes a close frame with optional code and reason.
pub fn wsCloseFrame(allocator: Allocator, code: WsCloseCode, reason: []const u8) ![]u8 {
    const code_u16: u16 = @intFromEnum(code);
    var payload_buf: [125]u8 = undefined;
    payload_buf[0] = @intCast((code_u16 >> 8) & 0xFF);
    payload_buf[1] = @intCast(code_u16 & 0xFF);
    const reason_len = @min(reason.len, 123);
    @memcpy(payload_buf[2..][0..reason_len], reason[0..reason_len]);
    return wsEncodeFrame(allocator, .close, payload_buf[0 .. 2 + reason_len], true, false, .{ 0, 0, 0, 0 });
}

// Frame decoding

/// Decodes one WebSocket frame from `data`.
///
/// Returns `error.NeedMoreData` if `data` is incomplete.
/// The returned `WsFrame.payload` is allocated; call `frame.deinit()` when done.
pub fn wsDecodeFrame(allocator: Allocator, data: []const u8) !WsDecodeResult {
    if (data.len < 2) return error.NeedMoreData;

    const fin = (data[0] & 0x80) != 0;
    const opcode: WsOpcode = @enumFromInt(data[0] & 0x0F);
    const masked = (data[1] & 0x80) != 0;
    var len: u64 = data[1] & 0x7F;
    var offset: usize = 2;

    if (len == 126) {
        if (data.len < 4) return error.NeedMoreData;
        len = (@as(u64, data[2]) << 8) | data[3];
        offset = 4;
    } else if (len == 127) {
        if (data.len < 10) return error.NeedMoreData;
        len = (@as(u64, data[2]) << 56) | (@as(u64, data[3]) << 48) |
            (@as(u64, data[4]) << 40) | (@as(u64, data[5]) << 32) |
            (@as(u64, data[6]) << 24) | (@as(u64, data[7]) << 16) |
            (@as(u64, data[8]) << 8) | data[9];
        offset = 10;
    }

    var mask: [4]u8 = .{ 0, 0, 0, 0 };
    if (masked) {
        if (data.len < offset + 4) return error.NeedMoreData;
        mask = data[offset..][0..4].*;
        offset += 4;
    }

    const payload_len: usize = @intCast(len);
    if (data.len < offset + payload_len) return error.NeedMoreData;

    const payload = try allocator.alloc(u8, payload_len);
    @memcpy(payload, data[offset..][0..payload_len]);

    if (masked) {
        for (payload, 0..) |*b, i| b.* ^= mask[i & 3];
    }

    return .{
        .frame = .{
            .fin = fin,
            .opcode = opcode,
            .masked = masked,
            .payload = payload,
            .allocator = allocator,
        },
        .consumed = offset + payload_len,
    };
}

// Tests

test "wsAcceptKey — RFC 6455 test vector" {
    const allocator = std.testing.allocator;
    const result = try wsAcceptKey("dGhlIHNhbXBsZSBub25jZQ==", allocator);
    defer allocator.free(result);
    try std.testing.expectEqualStrings("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", result);
}

test "wsEncodeFrame / wsDecodeFrame roundtrip — text" {
    const allocator = std.testing.allocator;
    const payload = "Hello, WebSocket!";
    const enc = try wsEncodeFrame(allocator, .text, payload, true, false, .{ 0, 0, 0, 0 });
    defer allocator.free(enc);
    var r = try wsDecodeFrame(allocator, enc);
    defer r.frame.deinit();
    try std.testing.expectEqual(WsOpcode.text, r.frame.opcode);
    try std.testing.expect(r.frame.fin);
    try std.testing.expectEqualStrings(payload, r.frame.payload);
}

test "wsEncodeFrame / wsDecodeFrame — masked" {
    const allocator = std.testing.allocator;
    const enc = try wsEncodeFrame(allocator, .text, "Hi", true, true, .{ 0x37, 0xfa, 0x21, 0x3d });
    defer allocator.free(enc);
    var r = try wsDecodeFrame(allocator, enc);
    defer r.frame.deinit();
    try std.testing.expectEqualStrings("Hi", r.frame.payload);
}

test "wsEncodeFrame — extended 16-bit length" {
    const allocator = std.testing.allocator;
    var big: [200]u8 = undefined;
    @memset(&big, 0xAB);
    const enc = try wsEncodeFrame(allocator, .binary, &big, true, false, .{ 0, 0, 0, 0 });
    defer allocator.free(enc);
    var r = try wsDecodeFrame(allocator, enc);
    defer r.frame.deinit();
    try std.testing.expectEqual(@as(usize, 200), r.frame.payload.len);
}

test "wsTextFrame / wsBinaryFrame convenience" {
    const allocator = std.testing.allocator;
    const tf = try wsTextFrame(allocator, "ping!");
    defer allocator.free(tf);
    const bf = try wsBinaryFrame(allocator, &.{ 1, 2, 3 });
    defer allocator.free(bf);
    try std.testing.expect(tf.len > 0);
    try std.testing.expect(bf.len > 0);
}

test "wsCloseFrame" {
    const allocator = std.testing.allocator;
    const cf = try wsCloseFrame(allocator, .normal, "bye");
    defer allocator.free(cf);
    // Decode and verify close opcode
    var r = try wsDecodeFrame(allocator, cf);
    defer r.frame.deinit();
    try std.testing.expectEqual(WsOpcode.close, r.frame.opcode);
}

test "isWebSocketUpgrade — valid" {
    const allocator = std.testing.allocator;
    var req = try @import("../core/request.zig").Request.init(allocator, .GET, "/ws");
    defer req.deinit();
    try req.headers.set("Upgrade", "websocket");
    try req.headers.set("Connection", "Upgrade");
    try req.headers.set("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
    try std.testing.expect(isWebSocketUpgrade(&req));
}

test "isWebSocketUpgrade — invalid (missing key)" {
    const allocator = std.testing.allocator;
    var req = try @import("../core/request.zig").Request.init(allocator, .GET, "/ws");
    defer req.deinit();
    try req.headers.set("Upgrade", "websocket");
    try req.headers.set("Connection", "Upgrade");
    try std.testing.expect(!isWebSocketUpgrade(&req));
}

test "WsOpcode classification" {
    try std.testing.expect(WsOpcode.text.isData());
    try std.testing.expect(WsOpcode.binary.isData());
    try std.testing.expect(!WsOpcode.ping.isData());
    try std.testing.expect(WsOpcode.ping.isControl());
    try std.testing.expect(WsOpcode.close.isControl());
}
