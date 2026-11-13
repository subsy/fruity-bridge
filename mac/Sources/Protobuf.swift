import Compression
import Foundation

struct BridgeError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

/// One decoded protobuf field. Length-delimited payloads keep a slice of the original bytes.
struct ProtoField {
    let number: Int
    let wireType: Int
    let varint: UInt64
    let data: ArraySlice<UInt8>
}

/// Minimal protobuf wire-format reader: enough for the Notes schemas (proto2, no packed fields).
struct ProtoMessage {
    let fields: [ProtoField]

    init(_ bytes: [UInt8]) throws { try self.init(bytes[...]) }

    init(_ bytes: ArraySlice<UInt8>) throws {
        var out: [ProtoField] = []
        var i = bytes.startIndex
        let end = bytes.endIndex
        func varint() throws -> UInt64 {
            var result: UInt64 = 0
            var shift: UInt64 = 0
            while true {
                guard i < end else { throw BridgeError("truncated varint") }
                let byte = bytes[i]
                i += 1
                if shift < 64 { result |= UInt64(byte & 0x7f) << shift }
                if byte & 0x80 == 0 { return result }
                shift += 7
                guard shift < 70 else { throw BridgeError("varint too long") }
            }
        }
        while i < end {
            let key = try varint()
            let number = Int(truncatingIfNeeded: key >> 3)
            let wire = Int(key & 7)
            guard number > 0 else { throw BridgeError("invalid field number") }
            switch wire {
            case 0:
                out.append(ProtoField(number: number, wireType: 0, varint: try varint(), data: []))
            case 1, 5:
                let size = wire == 1 ? 8 : 4
                guard end - i >= size else { throw BridgeError("truncated fixed field") }
                out.append(ProtoField(number: number, wireType: wire, varint: 0, data: bytes[i..<(i + size)]))
                i += size
            case 2:
                let length = try varint()
                guard length <= UInt64(end - i) else { throw BridgeError("truncated length-delimited field") }
                let next = i + Int(length)
                out.append(ProtoField(number: number, wireType: 2, varint: 0, data: bytes[i..<next]))
                i = next
            default:
                throw BridgeError("unsupported wire type \(wire)")
            }
        }
        fields = out
    }

    func first(_ number: Int) -> ProtoField? { fields.first { $0.number == number } }
    func all(_ number: Int) -> [ProtoField] { fields.filter { $0.number == number } }
    func uint(_ number: Int) -> UInt64? {
        guard let f = first(number), f.wireType == 0 else { return nil }
        return f.varint
    }
    /// int32/int64 fields: negative values are sign-extended to 64 bits on the wire.
    func int(_ number: Int) -> Int? { uint(number).map { Int(Int64(bitPattern: $0)) } }
    func bytes(_ number: Int) -> ArraySlice<UInt8>? {
        guard let f = first(number), f.wireType == 2 else { return nil }
        return f.data
    }
    func string(_ number: Int) -> String? { bytes(number).map { String(decoding: $0, as: UTF8.self) } }
    /// `float` fields (wire type 5, little-endian IEEE 754).
    func float(_ number: Int) -> Float? {
        guard let f = first(number), f.wireType == 5, f.data.count == 4 else { return nil }
        let bits = f.data.enumerated().reduce(UInt32(0)) { $0 | UInt32($1.element) << (8 * UInt32($1.offset)) }
        return Float(bitPattern: bits)
    }
    func message(_ number: Int) -> ProtoMessage? { bytes(number).flatMap { try? ProtoMessage($0) } }
    func messages(_ number: Int) -> [ProtoMessage] {
        all(number).compactMap { $0.wireType == 2 ? try? ProtoMessage($0.data) : nil }
    }
}

/// Protobuf writer used to synthesize fixtures in the self-tests.
struct ProtoWriter {
    var out: [UInt8] = []
    mutating func rawVarint(_ value: UInt64) {
        var v = value
        while v >= 0x80 {
            out.append(UInt8(v & 0x7f) | 0x80)
            v >>= 7
        }
        out.append(UInt8(v))
    }
    mutating func int(_ field: Int, _ value: Int) {
        rawVarint(UInt64(field << 3))
        rawVarint(UInt64(bitPattern: Int64(value)))
    }
    mutating func bytes(_ field: Int, _ data: [UInt8]) {
        rawVarint(UInt64(field << 3 | 2))
        rawVarint(UInt64(data.count))
        out += data
    }
    mutating func string(_ field: Int, _ value: String) { bytes(field, Array(value.utf8)) }
    mutating func float(_ field: Int, _ value: Float) {
        rawVarint(UInt64(field << 3 | 5))
        let bits = value.bitPattern
        out += (0..<4).map { UInt8(truncatingIfNeeded: bits >> (8 * UInt32($0))) }
    }
    mutating func message(_ field: Int, _ build: (inout ProtoWriter) -> Void) {
        var inner = ProtoWriter()
        build(&inner)
        bytes(field, inner.out)
    }
}

enum Gzip {
    /// Inflates gzip (RFC 1952) or zlib (RFC 1950) data using the Compression framework.
    static func inflate(_ input: [UInt8]) throws -> [UInt8] {
        var start = 0
        if input.count >= 18, input[0] == 0x1f, input[1] == 0x8b {
            guard input[2] == 8 else { throw BridgeError("unsupported gzip method") }
            let flags = input[3]
            var i = 10
            if flags & 0x04 != 0 {
                guard i + 2 <= input.count else { throw BridgeError("truncated gzip header") }
                i += 2 + (Int(input[i]) | Int(input[i + 1]) << 8)
            }
            if flags & 0x08 != 0 { while i < input.count, input[i] != 0 { i += 1 }; i += 1 }
            if flags & 0x10 != 0 { while i < input.count, input[i] != 0 { i += 1 }; i += 1 }
            if flags & 0x02 != 0 { i += 2 }
            guard i < input.count else { throw BridgeError("truncated gzip header") }
            start = i
        } else if input.count >= 2, input[0] & 0x0f == 8, (UInt16(input[0]) << 8 | UInt16(input[1])) % 31 == 0 {
            start = 2
        } else {
            throw BridgeError("not gzip or zlib data")
        }
        return try rawInflate(input[start...])
    }

    static func rawInflate(_ input: ArraySlice<UInt8>) throws -> [UInt8] {
        let stream = UnsafeMutablePointer<compression_stream>.allocate(capacity: 1)
        defer { stream.deallocate() }
        guard compression_stream_init(stream, COMPRESSION_STREAM_DECODE, COMPRESSION_ZLIB) == COMPRESSION_STATUS_OK else {
            throw BridgeError("inflate init failed")
        }
        defer { compression_stream_destroy(stream) }
        let capacity = 64 * 1024
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
        defer { buffer.deallocate() }
        var output: [UInt8] = []
        let source = Array(input)
        try source.withUnsafeBufferPointer { src in
            guard let base = src.baseAddress else { throw BridgeError("empty deflate stream") }
            stream.pointee.src_ptr = base
            stream.pointee.src_size = src.count
            while true {
                stream.pointee.dst_ptr = buffer
                stream.pointee.dst_size = capacity
                let status = compression_stream_process(stream, Int32(COMPRESSION_STREAM_FINALIZE.rawValue))
                let produced = capacity - stream.pointee.dst_size
                output.append(contentsOf: UnsafeBufferPointer(start: buffer, count: produced))
                guard output.count < 512 * 1024 * 1024 else { throw BridgeError("inflated data too large") }
                switch status {
                case COMPRESSION_STATUS_END: return
                case COMPRESSION_STATUS_OK:
                    if produced == 0 && stream.pointee.src_size == 0 { throw BridgeError("truncated deflate stream") }
                default: throw BridgeError("corrupt deflate stream")
                }
            }
        }
        return output
    }

    /// gzip-compresses `data` (used by self-tests to build fixtures).
    static func compress(_ data: [UInt8]) -> [UInt8] {
        let capacity = data.count + 1024
        var deflated = [UInt8](repeating: 0, count: capacity)
        let size = data.withUnsafeBufferPointer { src in
            deflated.withUnsafeMutableBufferPointer { dst in
                compression_encode_buffer(dst.baseAddress!, capacity, src.baseAddress!, data.count, nil, COMPRESSION_ZLIB)
            }
        }
        var out: [UInt8] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]
        out += deflated[0..<size]
        let crc = crc32(data)
        for shift in stride(from: 0, to: 32, by: 8) { out.append(UInt8((crc >> UInt32(shift)) & 0xff)) }
        let length = UInt32(truncatingIfNeeded: data.count)
        for shift in stride(from: 0, to: 32, by: 8) { out.append(UInt8((length >> UInt32(shift)) & 0xff)) }
        return out
    }

    static func crc32(_ data: [UInt8]) -> UInt32 {
        var crc: UInt32 = 0xffff_ffff
        for byte in data {
            crc ^= UInt32(byte)
            for _ in 0..<8 { crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xedb8_8320 : crc >> 1 }
        }
        return ~crc
    }
}
