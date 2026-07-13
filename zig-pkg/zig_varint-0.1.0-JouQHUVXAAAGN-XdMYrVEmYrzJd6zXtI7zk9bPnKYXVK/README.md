# zig-varint

Pure-Zig variable-length integer codecs shared across the `ch4r10t33r/zig-*` networking stack — `zquic`, `zig-libp2p`, `zig-ethp2p`, `zig-discv5`.

Two distinct encodings live here:

| Module                    | Encoding                                    | Used by                              |
|---------------------------|---------------------------------------------|--------------------------------------|
| `zig_varint.unsigned`     | Protobuf / multiformats / LEB128 (continuation-bit, up to 10 bytes for `u64`) | `zig-libp2p`, `zig-ethp2p`, `zig-discv5` |
| `zig_varint.quic`         | RFC 9000 §16 (top-2-bit length prefix, fixed 1/2/4/8 bytes, 62-bit values) | `zquic`                              |

The two encodings are **not wire-compatible** — pick the one that matches your protocol.

## Usage

`build.zig.zon`:

```zig
.dependencies = .{
    .zig_varint = .{
        .url = "https://github.com/ch4r10t33r/zig-varint/archive/refs/tags/v0.1.0.tar.gz",
        // .hash = "...",
    },
},
```

`build.zig`:

```zig
const varint_dep = b.dependency("zig_varint", .{ .target = target, .optimize = optimize });
my_module.addImport("zig_varint", varint_dep.module("zig_varint"));
```

In source:

```zig
const varint = @import("zig_varint");

// Multiformats / protobuf style
var scratch: [varint.unsigned.max_len]u8 = undefined;
const enc = varint.unsigned.encode(&scratch, 16384);
const dec = try varint.unsigned.decode(enc);

// QUIC style
var buf: [8]u8 = undefined;
const qenc = try varint.quic.encode(&buf, 16384);
const qdec = try varint.quic.decode(qenc);
```

## API surface (unsigned)

| Symbol                              | Purpose                                                |
|-------------------------------------|--------------------------------------------------------|
| `max_len` / `max_encoding_bytes`    | 10 (max bytes for a `u64`)                             |
| `encodedLen(value) -> usize`        | Bytes the minimal encoding occupies                    |
| `encode(*[max_len]u8, u64) -> []`   | Minimal encoding into a fixed-size scratch buffer      |
| `encodeToScratch(*[max_encoding_bytes]u8, usize)` | Backward-compat alias used by zig-libp2p   |
| `append(*ArrayList, gpa, u64)`      | Push onto a growable buffer                            |
| `decode(slice) -> {value, len}`     | Strict; rejects non-minimal encodings                  |
| `decodeRelaxed(slice)`              | Accepts non-minimal forms                              |
| `decodeAt(buf, *offset) -> u64`     | Cursor-style decode used by zig-ethp2p                 |
| `decodeAtRelaxed(buf, *offset)`     | Cursor + relaxed minimality                            |
| `decodeNonNegativeI32(buf, *offset)`| Protobuf `int32` for known non-negative values         |
| `DecodeError`                       | `{ Truncated, Overflow, TooLong, NonMinimal }`         |

## API surface (quic)

| Symbol                              | Purpose                                                |
|-------------------------------------|--------------------------------------------------------|
| `max_value`                         | `(1 << 62) - 1`                                        |
| `encodedLen(u64) -> u4`             | 1 / 2 / 4 / 8 (per RFC 9000 §16)                       |
| `encode(buf, u64) -> []u8`          | Returns the slice written (1, 2, 4, or 8 bytes)        |
| `decode(buf) -> {value, len}`       | Rejects non-minimal encodings (RFC 9000 §16 MUST)      |
| `lenToUsize(u64) -> usize`          | Safe cast of a varint-decoded length                   |
| `Reader`, `Writer`                  | Cursor-style helpers                                   |
| `EncodeError`                       | `{ ValueTooLarge }`                                    |
| `DecodeError`                       | `{ BufferTooShort, VarintLengthTooLarge, NonMinimalEncoding }` |

## Testing

```
$ zig build test
```

Tests cover boundary values, round-trips, non-minimal rejection, overflow, and truncation for both encodings.

## License

MIT.
