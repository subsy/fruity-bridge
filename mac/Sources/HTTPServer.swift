import Darwin
import Foundation

struct HTTPRequest {
    var method: String
    /// Raw (still percent-encoded) path without the query string.
    var rawPath: String
    var query: [String: String]
    var headers: [String: String] // lowercased names
    var body: Data

    func header(_ name: String) -> String? { headers[name.lowercased()] }

    func jsonBody() throws -> [String: Any] {
        guard !body.isEmpty else { return [:] }
        guard let object = try? JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed]), let dict = object as? [String: Any] else {
            throw APIError.badRequest("The request body must be a JSON object.")
        }
        return dict
    }
}

struct HTTPResponse {
    var status: Int
    var contentType = "application/json; charset=utf-8"
    var body = Data()
    var fileURL: URL?
    var isEventStream = false

    static func json(_ object: Any, status: Int = 200) -> HTTPResponse {
        HTTPResponse(status: status, body: jsonData(object))
    }
    static func error(_ error: APIError) -> HTTPResponse {
        var payload: [String: Any] = ["code": error.code, "message": error.message]
        if let current = error.current { payload["current"] = current }
        return .json(["error": payload], status: error.status)
    }
    static let noContent = HTTPResponse(status: 204, contentType: "")
    static let eventStream = HTTPResponse(status: 200, contentType: "text/event-stream", isEventStream: true)
}

enum HTTPParser {
    enum Head {
        case incomplete
        /// `status` is the response to send before closing the connection.
        case invalid(status: Int, reason: String)
        /// The request without its body; `bodyLength` bytes follow the `headerLength` bytes of head.
        case request(HTTPRequest, headerLength: Int, bodyLength: Int)
    }
    enum Outcome { case incomplete, invalid(String), request(HTTPRequest, consumed: Int) }

    static let maxHeaderBytes = 16 * 1024
    static let maxBodyBytes = 1024 * 1024

    /// Parses the request line and headers at the front of `buffer`.
    static func parseHead(_ buffer: [UInt8]) -> Head {
        guard let headerEnd = find(buffer, [13, 10, 13, 10], limit: maxHeaderBytes + 4) else {
            return buffer.count >= maxHeaderBytes + 4 ? .invalid(status: 431, reason: "headers too large") : .incomplete
        }
        guard headerEnd <= maxHeaderBytes else { return .invalid(status: 431, reason: "headers too large") }
        let head = String(decoding: buffer[0..<headerEnd], as: UTF8.self)
        var lines = head.components(separatedBy: "\r\n")
        let requestLine = lines.removeFirst().split(separator: " ", omittingEmptySubsequences: true)
        guard requestLine.count == 3, requestLine[2].hasPrefix("HTTP/1.") else { return .invalid(status: 400, reason: "bad request line") }
        var headers: [String: String] = [:]
        for line in lines where !line.isEmpty {
            guard let colon = line.firstIndex(of: ":") else { return .invalid(status: 400, reason: "bad header") }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            headers[name] = headers[name].map { "\($0), \(value)" } ?? value
        }
        if let encoding = headers["transfer-encoding"], encoding.lowercased() != "identity" {
            return .invalid(status: 400, reason: "chunked request bodies are not supported")
        }
        guard let length = Int(headers["content-length"] ?? "0"), length >= 0 else { return .invalid(status: 400, reason: "bad content length") }
        guard length <= maxBodyBytes else { return .invalid(status: 413, reason: "body larger than 1 MB") }
        let target = String(requestLine[1])
        let pathPart: String
        var query: [String: String] = [:]
        if let mark = target.firstIndex(of: "?") {
            pathPart = String(target[..<mark])
            for pair in target[target.index(after: mark)...].split(separator: "&") {
                let kv = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
                let key = decodeQuery(String(kv[0]))
                query[key] = kv.count > 1 ? decodeQuery(String(kv[1])) : ""
            }
        } else {
            pathPart = target
        }
        let request = HTTPRequest(method: String(requestLine[0]).uppercased(), rawPath: pathPart, query: query, headers: headers, body: Data())
        return .request(request, headerLength: headerEnd + 4, bodyLength: length)
    }

    /// Parses one whole HTTP/1.1 request (head and body) from the front of `buffer`.
    static func parse(_ buffer: [UInt8]) -> Outcome {
        switch parseHead(buffer) {
        case .incomplete: return .incomplete
        case .invalid(_, let reason): return .invalid(reason)
        case .request(var request, let headerLength, let bodyLength):
            guard buffer.count >= headerLength + bodyLength else { return .incomplete }
            request.body = Data(buffer[headerLength..<(headerLength + bodyLength)])
            return .request(request, consumed: headerLength + bodyLength)
        }
    }

    static func decodeQuery(_ text: String) -> String {
        text.replacingOccurrences(of: "+", with: " ").removingPercentEncoding ?? text
    }

    /// First index of `needle` among the first `limit` bytes of `haystack`.
    static func find(_ haystack: [UInt8], _ needle: [UInt8], limit: Int = .max) -> Int? {
        let end = min(haystack.count, limit)
        guard end >= needle.count else { return nil }
        var i = 0
        while i <= end - needle.count {
            if haystack[i] == needle[0], Array(haystack[i..<(i + needle.count)]) == needle { return i }
            i += 1
        }
        return nil
    }

    static func reason(_ status: Int) -> String {
        switch status {
        case 200: return "OK"
        case 201: return "Created"
        case 204: return "No Content"
        case 400: return "Bad Request"
        case 401: return "Unauthorized"
        case 403: return "Forbidden"
        case 404: return "Not Found"
        case 405: return "Method Not Allowed"
        case 408: return "Request Timeout"
        case 409: return "Conflict"
        case 413: return "Content Too Large"
        case 422: return "Unprocessable Entity"
        case 431: return "Request Header Fields Too Large"
        case 500: return "Internal Server Error"
        case 503: return "Service Unavailable"
        default: return "Status"
        }
    }
}

/// Reads requests from a blocking socket with whole-phase deadlines (not per-recv timeouts, so
/// a client trickling one byte at a time can't hold a connection open).
struct RequestReader {
    enum Result {
        case head(HTTPRequest, bodyLength: Int)
        case body(Data)
        /// The peer closed, or nothing arrived before the deadline on an idle connection.
        case closed
        /// Reply with this and close the connection.
        case refuse(HTTPResponse)
    }

    static let headerTimeout: TimeInterval = 10
    static let bodyTimeout: TimeInterval = 30

    let fd: Int32
    var buffer: [UInt8] = []

    init(fd: Int32) { self.fd = fd }

    /// Request line and headers, all within `timeout` (≤ 16 KB).
    mutating func readHead(timeout: TimeInterval = RequestReader.headerTimeout) -> Result {
        let deadline = Date().addingTimeInterval(timeout)
        while true {
            switch HTTPParser.parseHead(buffer) {
            case .request(let request, let headerLength, let bodyLength):
                buffer.removeFirst(headerLength)
                return .head(request, bodyLength: bodyLength)
            case .invalid(let status, let reason):
                return .refuse(.error(APIError(status: status, code: "bad_request", message: "Malformed request: \(reason).")))
            case .incomplete:
                switch receive(until: deadline) {
                case .data: continue
                case .closed: return .closed
                case .timedOut:
                    return buffer.isEmpty ? .closed : .refuse(.error(APIError(status: 408, code: "bad_request", message: "Request headers took longer than \(Int(timeout)) s.")))
                }
            }
        }
    }

    /// Exactly `length` body bytes, all within `timeout`.
    mutating func readBody(length: Int, timeout: TimeInterval = RequestReader.bodyTimeout) -> Result {
        let deadline = Date().addingTimeInterval(timeout)
        while buffer.count < length {
            switch receive(until: deadline) {
            case .data: continue
            case .closed: return .closed
            case .timedOut: return .refuse(.error(APIError(status: 408, code: "bad_request", message: "The request body took too long.")))
            }
        }
        let body = Data(buffer[0..<length])
        buffer.removeFirst(length)
        return .body(body)
    }

    private enum Received { case data, closed, timedOut }

    private mutating func receive(until deadline: Date) -> Received {
        var chunk = [UInt8](repeating: 0, count: 16 * 1024)
        while true {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { return .timedOut }
            var tv = timeval(tv_sec: Int(remaining), tv_usec: Int32(max(1000, (remaining - floor(remaining)) * 1_000_000)))
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
            let count = chunk.withUnsafeMutableBytes { recv(fd, $0.baseAddress, $0.count, 0) }
            if count > 0 {
                buffer += chunk[0..<count]
                return .data
            }
            if count == 0 { return .closed }
            if errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK { continue }
            return .closed
        }
    }
}

/// Blocking-socket HTTP/1.1 server bound to 127.0.0.1, one thread per connection.
final class HTTPServer {
    static let maxConnections = 32

    let port: UInt16
    let events = EventHub()
    /// Runs on the request line and headers, before the body is read; a response refuses the request.
    private let authorize: (inout HTTPRequest) -> HTTPResponse?
    private let handler: (HTTPRequest) -> HTTPResponse
    private var listenSocket: Int32 = -1
    private let active = Locked(0)

    init(port: UInt16, authorize: @escaping (inout HTTPRequest) -> HTTPResponse?, handler: @escaping (HTTPRequest) -> HTTPResponse) {
        self.port = port
        self.authorize = authorize
        self.handler = handler
    }

    convenience init(port: UInt16, router: Router) {
        self.init(port: port, authorize: router.authorize, handler: router.dispatch)
    }

    func start() throws {
        signal(SIGPIPE, SIG_IGN)
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw BridgeError("socket() failed: \(String(cString: strerror(errno)))") }
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0 else {
            let message = String(cString: strerror(errno))
            close(fd)
            throw BridgeError("Port \(port) unavailable: \(message)")
        }
        guard listen(fd, 64) == 0 else {
            close(fd)
            throw BridgeError("listen() failed")
        }
        listenSocket = fd
        events.start()
        let thread = Thread { [weak self] in self?.acceptLoop() }
        thread.name = "http-accept"
        thread.start()
    }

    private func acceptLoop() {
        while true {
            let client = accept(listenSocket, nil, nil)
            if client < 0 {
                if errno == EINTR || errno == ECONNABORTED { continue }
                usleep(100_000)
                continue
            }
            var yes: Int32 = 1
            setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &yes, socklen_t(MemoryLayout<Int32>.size))
            setTimeout(client, SO_SNDTIMEO, seconds: 30)
            let admitted = active.update { count -> Bool in
                guard count < HTTPServer.maxConnections else { return false }
                count += 1
                return true
            }
            guard admitted else {
                setTimeout(client, SO_SNDTIMEO, seconds: 1)
                _ = write(client, response: .error(APIError(status: 503, code: "internal", message: "Too many connections; try again shortly.")), keepAlive: false)
                close(client)
                continue
            }
            Thread.detachNewThread { [weak self] in
                self?.serve(client)
                self?.active.update { $0 -= 1 }
            }
        }
    }

    private func setTimeout(_ fd: Int32, _ option: Int32, seconds: Int) {
        var tv = timeval(tv_sec: seconds, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, option, &tv, socklen_t(MemoryLayout<timeval>.size))
    }

    private func serve(_ fd: Int32) {
        var reader = RequestReader(fd: fd)
        connection: while true {
            var request: HTTPRequest
            var bodyLength = 0
            switch reader.readHead() {
            case .head(let head, let length):
                request = head
                bodyLength = length
            case .refuse(let response):
                _ = write(fd, response: response, keepAlive: false)
                break connection
            case .closed, .body:
                break connection
            }
            // Browser, Host and identity checks before any body byte is read.
            if let refusal = authorize(&request) {
                _ = write(fd, response: refusal, keepAlive: false, headOnly: request.method == "HEAD")
                break connection
            }
            if bodyLength > 0 {
                switch reader.readBody(length: bodyLength) {
                case .body(let body): request.body = body
                case .refuse(let response):
                    _ = write(fd, response: response, keepAlive: false)
                    break connection
                case .closed, .head:
                    break connection
                }
            }
            let response = handler(request)
            if response.isEventStream && response.status == 200 {
                streamEvents(fd)
                return
            }
            let keepAlive = request.header("connection")?.lowercased() != "close"
            guard write(fd, response: response, keepAlive: keepAlive, headOnly: request.method == "HEAD"), keepAlive else { break connection }
        }
        close(fd)
    }

    private func streamEvents(_ fd: Int32) {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n: connected\n\n"
        guard HTTPServer.sendAll(fd, Data(head.utf8)) else { close(fd); return }
        setTimeout(fd, SO_RCVTIMEO, seconds: 0)
        let client = events.add(fd)
        // Block until the peer closes (or the hub evicts this stream); writes happen on the hub's queue.
        var scratch = [UInt8](repeating: 0, count: 1024)
        while true {
            let count = scratch.withUnsafeMutableBytes { recv(fd, $0.baseAddress, $0.count, 0) }
            if count < 0 && errno == EINTR { continue }
            if count <= 0 { break }
        }
        events.remove(client)
    }

    private func write(_ fd: Int32, response: HTTPResponse, keepAlive: Bool, headOnly: Bool = false) -> Bool {
        var body = response.body
        if let file = response.fileURL {
            guard let data = try? Data(contentsOf: file, options: .mappedIfSafe) else {
                return write(fd, response: .error(.notFound("Attachment not readable.")), keepAlive: keepAlive)
            }
            body = data
        }
        var head = "HTTP/1.1 \(response.status) \(HTTPParser.reason(response.status))\r\n"
        if !response.contentType.isEmpty { head += "Content-Type: \(response.contentType)\r\n" }
        head += "Content-Length: \(body.count)\r\nCache-Control: no-store\r\nConnection: \(keepAlive ? "keep-alive" : "close")\r\n\r\n"
        guard HTTPServer.sendAll(fd, Data(head.utf8)) else { return false }
        return headOnly || body.isEmpty || HTTPServer.sendAll(fd, body)
    }

    static func sendAll(_ fd: Int32, _ data: Data) -> Bool {
        data.withUnsafeBytes { raw -> Bool in
            guard var pointer = raw.baseAddress else { return true }
            var remaining = raw.count
            while remaining > 0 {
                let sent = send(fd, pointer, remaining, 0)
                if sent < 0 && errno == EINTR { continue }
                if sent <= 0 { return false }
                remaining -= sent
                pointer += sent
            }
            return true
        }
    }
}

/// Server-sent events fan-out with a 20 s keep-alive ping; at most 8 streams (oldest is closed).
final class EventHub {
    static let maxStreams = 8

    final class Client {
        let fd: Int32
        let queue: DispatchQueue
        var closed = false
        init(fd: Int32) {
            self.fd = fd
            queue = DispatchQueue(label: "sse-\(fd)")
        }
    }

    /// Oldest first.
    private let clients = Locked<[Client]>([])
    private var timer: DispatchSourceTimer?
    private var debounce: [String: DispatchWorkItem] = [:]
    private let debounceQueue = DispatchQueue(label: "sse-debounce")

    var clientCount: Int { clients.get().count }

    func start() {
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue.global())
        timer.schedule(deadline: .now() + 20, repeating: 20)
        timer.setEventHandler { [weak self] in self?.send(": ping\n\n") }
        timer.resume()
        self.timer = timer
    }

    func add(_ fd: Int32) -> Client {
        let client = Client(fd: fd)
        let evicted = clients.update { list -> [Client] in
            list.append(client)
            let excess = max(0, list.count - EventHub.maxStreams)
            let oldest = Array(list.prefix(excess))
            list.removeFirst(excess)
            return oldest
        }
        // Shutting the socket down wakes the stream's own thread, which then calls remove().
        for old in evicted { old.queue.async { if !old.closed { shutdown(old.fd, SHUT_RDWR) } } }
        return client
    }

    /// Called by the stream's thread once its socket is done; closes the descriptor.
    func remove(_ client: Client) {
        clients.update { $0.removeAll { $0 === client } }
        client.queue.async {
            guard !client.closed else { return }
            client.closed = true
            shutdown(client.fd, SHUT_RDWR)
            close(client.fd)
        }
    }

    func publish(_ event: String, changedAt: Date = Date()) {
        let payload = String(decoding: jsonData(["changedAt": RFC3339.string(changedAt)]), as: UTF8.self)
        send("event: \(event)\ndata: \(payload)\n\n")
    }

    /// Publishes after `delay` of quiet, coalescing bursts (EKEventStoreChanged fires in bursts).
    func publishDebounced(_ event: String, delay: TimeInterval) {
        debounceQueue.async {
            self.debounce[event]?.cancel()
            let item = DispatchWorkItem { [weak self] in self?.publish(event) }
            self.debounce[event] = item
            self.debounceQueue.asyncAfter(deadline: .now() + delay, execute: item)
        }
    }

    private func send(_ text: String) {
        let data = Data(text.utf8)
        for client in clients.get() {
            client.queue.async {
                guard !client.closed else { return }
                if !HTTPServer.sendAll(client.fd, data) { shutdown(client.fd, SHUT_RDWR) }
            }
        }
    }
}
