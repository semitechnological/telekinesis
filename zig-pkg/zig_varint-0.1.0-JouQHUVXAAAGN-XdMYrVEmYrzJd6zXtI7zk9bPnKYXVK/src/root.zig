//! `zig-varint` — pure-Zig variable-length integer codecs shared across the
//! ch4r10t33r/zig-* networking stack.
//!
//! Two distinct encodings live here:
//!
//! * `unsigned` — protobuf / multiformats / LEB128 style (continuation-bit,
//!   variable length, up to 10 bytes for `u64`).  Used by zig-libp2p,
//!   zig-ethp2p, zig-discv5 and any other consumer that speaks libp2p,
//!   protobuf, or the multiformats unsigned-varint.
//!
//! * `quic` — RFC 9000 §16 (top-2-bit length prefix, fixed 1/2/4/8-byte
//!   sizes, 62-bit values).  Used by zquic.
//!
//! Pick one — they are *not* wire-compatible.

pub const unsigned = @import("unsigned.zig");
pub const quic = @import("quic.zig");

test {
    _ = @import("unsigned.zig");
    _ = @import("quic.zig");
}
