//! Unsigned LEB128 / multiformats unsigned-varint / protobuf varint, for `u64`.
//!
//! Wire format: 7-bit groups, low byte first; continuation bit `0x80` set on
//! every byte except the last.  Maximum length for a `u64` value is 10 bytes
//! (9 × 7 = 63 bits + 1 continuation byte → 10).
//!
//! This single module is the superset of three previously-separate copies in
//! `zquic` (only QUIC varint was used there — see `quic.zig`), `zig-ethp2p`,
//! `zig-libp2p`, and `zig-discv5`.  It exposes:
//!
//! * `decode(slice) → DecodeResult` — strict, rejects non-minimal encodings.
//! * `decodeRelaxed(slice) → DecodeResult` — accepts non-minimal forms (for
//!   peers that emit them — multiformats spec is ambiguous here).
//! * `decodeAt(buf, *offset) → u64` — ethp2p-style cursor; advances `offset`.
//! * `decodeAtRelaxed(buf, *offset) → u64` — same, relaxed minimality.
//! * `decodeNonNegativeI32(buf, *offset) → i32` — protobuf `int32` for known
//!   non-negative ranges (e.g. RS preamble counters).
//! * `encode(*[max_len]u8, value) → []const u8` — minimal encoding.
//! * `encodedLen(value) → usize`.
//! * `append(*std.ArrayList(u8), gpa, value)` — push to a growable buffer.
//! * `encodeToScratch(*[max_encoding_bytes]u8, usize) → []const u8` — alias
//!   for `encode` matching the previous libp2p name.

const std = @import("std");

/// Maximum bytes a `u64` value occupies (`ceil(64 / 7) = 10`).
pub const max_len: usize = 10;

/// Backward-compatible alias used by zig-libp2p.
pub const max_encoding_bytes: usize = max_len;

pub const DecodeError = error{
    /// Buffer ran out before the continuation bit was cleared.
    Truncated,
    /// Decoded value would not fit in `u64`.
    Overflow,
    /// Encoded sequence exceeded `max_len` bytes (dummy continuation bytes).
    TooLong,
    /// Sequence was longer than the minimal encoding for its value (rejected
    /// by the strict `decode`; tolerated by `decodeRelaxed`).
    NonMinimal,
};

pub const DecodeResult = struct { value: u64, len: usize };

/// Number of bytes the minimal encoding of `value` occupies.
pub fn encodedLen(value: u64) usize {
    var v = value;
    var n: usize = 1;
    while (v >= 0x80) : (n += 1) v >>= 7;
    return n;
}

/// Encode `value` into the front of `buf` and return the used prefix.
pub fn encode(buf: *[max_len]u8, value: u64) []const u8 {
    var v = value;
    var i: usize = 0;
    while (i < max_len) : (i += 1) {
        const byte: u8 = @truncate(v & 0x7f);
        v >>= 7;
        if (v == 0) {
            buf[i] = byte;
            return buf[0 .. i + 1];
        }
        buf[i] = byte | 0x80;
    }
    unreachable;
}

/// Backward-compatible alias used by zig-libp2p.  `value` is `usize` for
/// API parity with the original signature; on 64-bit targets this is `u64`.
pub fn encodeToScratch(scratch: *[max_encoding_bytes]u8, value: usize) []const u8 {
    return encode(scratch, value);
}

/// Append the minimal encoding of `value` to a growable buffer.
pub fn append(dst: *std.ArrayList(u8), gpa: std.mem.Allocator, value: u64) std.mem.Allocator.Error!void {
    var scratch: [max_len]u8 = undefined;
    const enc = encode(&scratch, value);
    try dst.appendSlice(gpa, enc);
}

/// Strict decode — rejects non-minimal encodings (e.g. `0x80 0x00`).
pub fn decode(slice: []const u8) DecodeError!DecodeResult {
    return decodeImpl(slice, .strict);
}

/// Relaxed decode — accepts non-minimal encodings; everything else identical.
pub fn decodeRelaxed(slice: []const u8) DecodeError!DecodeResult {
    return decodeImpl(slice, .relaxed);
}

const Strictness = enum { strict, relaxed };

fn decodeImpl(slice: []const u8, strict: Strictness) DecodeError!DecodeResult {
    if (slice.len == 0) return error.Truncated;

    var result: u64 = 0;
    var shift: u6 = 0;

    for (0..max_len) |idx| {
        if (idx >= slice.len) return error.Truncated;
        const b = slice[idx];
        const digit: u64 = b & 0x7f;

        if (shift == 63 and digit > 1) return error.Overflow;

        const shifted = digit << @as(u6, @intCast(shift));
        const ov = @addWithOverflow(result, shifted);
        if (ov[1] != 0) return error.Overflow;
        result = ov[0];

        if (b & 0x80 == 0) {
            if (strict == .strict) {
                var enc: [max_len]u8 = undefined;
                const enc_slice = encode(&enc, result);
                if (enc_slice.len != idx + 1 or !std.mem.eql(u8, enc_slice, slice[0 .. idx + 1])) {
                    return error.NonMinimal;
                }
            }
            return .{ .value = result, .len = idx + 1 };
        }

        if (shift == 63) return error.TooLong;
        shift += 7;
    }

    return error.TooLong;
}

/// Cursor-style decode used by zig-ethp2p.  Reads a varint at `offset`,
/// advances `offset` past it, returns the value.  Strict-minimal.
pub fn decodeAt(buf: []const u8, offset: *usize) DecodeError!u64 {
    const r = try decode(buf[offset.*..]);
    offset.* += r.len;
    return r.value;
}

/// Same as `decodeAt` but accepts non-minimal encodings.
pub fn decodeAtRelaxed(buf: []const u8, offset: *usize) DecodeError!u64 {
    const r = try decodeRelaxed(buf[offset.*..]);
    offset.* += r.len;
    return r.value;
}

/// Protobuf `int32` for known-non-negative ranges.  Returns
/// `error.Overflow` when the decoded value exceeds `std.math.maxInt(i32)`.
pub fn decodeNonNegativeI32(buf: []const u8, offset: *usize) DecodeError!i32 {
    const u = try decodeAt(buf, offset);
    if (u > @as(u64, @intCast(std.math.maxInt(i32)))) return error.Overflow;
    return @intCast(u);
}

// ── tests ────────────────────────────────────────────────────────────────────

const t = std.testing;

test "encode/decode round-trip across boundary values" {
    const cases = [_]u64{ 0, 1, 127, 128, 16383, 16384, 1 << 40, std.math.maxInt(u64) };
    for (cases) |v| {
        var buf: [max_len]u8 = undefined;
        const enc = encode(&buf, v);
        const dec = try decode(enc);
        try t.expectEqual(v, dec.value);
        try t.expectEqual(enc.len, dec.len);
        try t.expectEqual(enc.len, encodedLen(v));
    }
}

test "decode rejects truncated" {
    try t.expectError(error.Truncated, decode(&[_]u8{0x80}));
    try t.expectError(error.Truncated, decode(&[_]u8{}));
}

test "decode rejects too-long sequence" {
    const bad = [_]u8{ 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02 };
    try t.expectError(error.TooLong, decode(&bad));
}

test "decode rejects overflow" {
    const bytes = [_]u8{ 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x03 };
    try t.expectError(error.Overflow, decode(&bytes));
}

test "decode strict rejects non-minimal; relaxed accepts" {
    const bytes = [_]u8{ 0x80, 0x00 }; // value=0 in 2 bytes
    try t.expectError(error.NonMinimal, decode(&bytes));
    const r = try decodeRelaxed(&bytes);
    try t.expectEqual(@as(u64, 0), r.value);
    try t.expectEqual(@as(usize, 2), r.len);
}

test "append + decodeAt round-trip" {
    var list: std.ArrayList(u8) = .empty;
    defer list.deinit(t.allocator);
    try append(&list, t.allocator, 1);
    try append(&list, t.allocator, 200);
    try append(&list, t.allocator, 16384);
    var off: usize = 0;
    try t.expectEqual(@as(u64, 1), try decodeAt(list.items, &off));
    try t.expectEqual(@as(u64, 200), try decodeAt(list.items, &off));
    try t.expectEqual(@as(u64, 16384), try decodeAt(list.items, &off));
    try t.expectEqual(list.items.len, off);
}

test "decodeNonNegativeI32 overflow" {
    var buf: [max_len]u8 = undefined;
    const enc = encode(&buf, @as(u64, std.math.maxInt(i32)) + 1);
    var off: usize = 0;
    try t.expectError(error.Overflow, decodeNonNegativeI32(enc, &off));
}

test "encodeToScratch alias matches encode" {
    var b1: [max_len]u8 = undefined;
    var b2: [max_encoding_bytes]u8 = undefined;
    const a = encode(&b1, 16384);
    const b = encodeToScratch(&b2, 16384);
    try t.expectEqualSlices(u8, a, b);
}
