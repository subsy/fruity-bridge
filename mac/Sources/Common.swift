import Foundation
import Security

let bridgeVersion = "0.1.0"
let bridgePort: UInt16 = 47380
/// LaunchAgent label (install.sh writes ~/Library/LaunchAgents/<label>.plist); also the bundle id.
let agentLabel = "com.plgeek.fruitybridge"

/// An error the HTTP layer turns into `{"error": {"code", "message", "current"?}}`.
struct APIError: Error {
    var status: Int
    var code: String
    var message: String
    var current: Any?

    static func badRequest(_ message: String) -> APIError { APIError(status: 400, code: "bad_request", message: message) }
    static func notFound(_ message: String = "Not found.") -> APIError { APIError(status: 404, code: "not_found", message: message) }
    static func conflict(_ message: String, current: Any?) -> APIError { APIError(status: 409, code: "conflict", message: message, current: current) }
    static func notEditable(_ message: String) -> APIError { APIError(status: 422, code: "not_editable", message: message) }
    static func permission(_ message: String) -> APIError { APIError(status: 503, code: "permission_missing", message: message) }
    static func unauthorized(_ message: String) -> APIError { APIError(status: 401, code: "unauthorized", message: message) }
    static func forbidden(_ message: String) -> APIError { APIError(status: 403, code: "forbidden", message: message) }
    static func internalError(_ message: String) -> APIError { APIError(status: 500, code: "internal", message: message) }
}

/// The one wording for each missing macOS permission (`permission_missing` messages).
enum PermissionMessage {
    static let reminders = "Reminders access isn't allowed. On the Mac, use the Fruity Bridge menu → Allow Reminders access…, or System Settings → Privacy & Security → Reminders."
    static let notesDatabase = "Fruity Bridge can't read the Notes database. On the Mac: System Settings → Privacy & Security → Full Disk Access → turn on Fruity Bridge."
    static let notesAutomation = "Fruity Bridge isn't allowed to control Notes. On the Mac, use the Fruity Bridge menu → Allow Notes editing…, or System Settings → Privacy & Security → Automation → Fruity Bridge → Notes."
    static let notesWritesUnavailable = "Editing notes needs the Fruity Bridge menu-bar app with Automation access to Notes."
}

enum RFC3339 {
    private static let formatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        f.timeZone = TimeZone(identifier: "UTC")
        return f
    }()
    private static let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    static func string(_ date: Date?) -> Any { date.map { formatter.string(from: $0) } ?? NSNull() }
    static func parse(_ text: String) -> Date? { formatter.date(from: text) ?? fractional.date(from: text) }

    /// Timestamps travel with whole-second precision, so conflicts compare whole seconds.
    static func sameSecond(_ a: Date?, _ b: Date?) -> Bool {
        guard let a, let b else { return a == nil && b == nil }
        return floor(a.timeIntervalSince1970) == floor(b.timeIntervalSince1970)
    }
}

struct BridgeConfig: Codable, Equatable {
    /// Tailscale logins allowed to use the bridge. Empty refuses everyone; install.sh adds the
    /// login this Mac's Tailscale is signed in with.
    var allowedLogins: [String] = []

    func allows(_ login: String) -> Bool {
        allowedLogins.contains { $0.caseInsensitiveCompare(login) == .orderedSame }
    }
    /// Shown and copied by the menu; install.sh fills it in from `tailscale status`.
    var tailnetURL: String?
    /// Random path segment `tailscale serve` prepends to every proxied request (install.sh sets
    /// the mapping). Only requests carrying it may use the Tailscale-User-Login header, so a process
    /// that connects to 127.0.0.1 directly can't pose as a tailnet user unless it can read this
    /// file (0600) or `tailscale serve status`, which processes running as the same user can.
    var pathSecret: String?
    /// Set when the user chooses "Read Notes database now…" without Full Disk Access (macOS may then
    /// have asked to allow access to Notes' data instead).
    var notesDatabaseApproved: Bool?

    static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/Fruity Bridge", isDirectory: true)
    }

    static func load(from directory: URL = BridgeConfig.directory, createIfMissing: Bool) -> BridgeConfig {
        let file = directory.appendingPathComponent("config.json")
        if let data = try? Data(contentsOf: file), let config = try? JSONDecoder().decode(BridgeConfig.self, from: data) {
            return config
        }
        let config = BridgeConfig()
        if createIfMissing, !FileManager.default.fileExists(atPath: file.path) { try? config.save(to: directory) }
        return config
    }

    func save(to directory: URL = BridgeConfig.directory) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(self)
        // The file holds the path secret: write it 0600 from the start, then rename into place.
        let file = directory.appendingPathComponent("config.json").path
        let temporary = file + ".tmp-\(getpid())"
        let fd = open(temporary, O_WRONLY | O_CREAT | O_TRUNC | O_EXCL, 0o600)
        guard fd >= 0 else { throw BridgeError("Couldn't write \(temporary): \(String(cString: strerror(errno)))") }
        let written = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        let synced = fsync(fd) == 0
        close(fd)
        guard written == data.count, synced, rename(temporary, file) == 0 else {
            unlink(temporary)
            throw BridgeError("Couldn't save \(file).")
        }
        chmod(file, 0o600)
        chmod(directory.path, 0o700)
    }

    /// Returns the path secret, creating and saving one if the config has none.
    static func ensurePathSecret(in directory: URL = BridgeConfig.directory) throws -> String {
        var config = BridgeConfig.load(from: directory, createIfMissing: false)
        if let secret = config.pathSecret, !secret.isEmpty {
            try config.save(to: directory) // re-applies 0600 to configs written by older builds
            return secret
        }
        var bytes = [UInt8](repeating: 0, count: 24)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { throw BridgeError("No random bytes available.") }
        let secret = bytes.map { String(format: "%02x", $0) }.joined()
        config.pathSecret = secret
        try config.save(to: directory)
        return secret
    }

    init() {}
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        allowedLogins = try c.decodeIfPresent([String].self, forKey: .allowedLogins) ?? []
        tailnetURL = try c.decodeIfPresent(String.self, forKey: .tailnetURL)
        pathSecret = try c.decodeIfPresent(String.self, forKey: .pathSecret)
        notesDatabaseApproved = try c.decodeIfPresent(Bool.self, forKey: .notesDatabaseApproved)
    }
}

func jsonData(_ value: Any) -> Data {
    (try? JSONSerialization.data(withJSONObject: value, options: [.withoutEscapingSlashes])) ?? Data("{}".utf8)
}

/// Thread-safe holder for a value shared between the HTTP threads and the main thread.
final class Locked<Value> {
    private var value: Value
    private let lock = NSLock()
    init(_ value: Value) { self.value = value }
    func get() -> Value { lock.lock(); defer { lock.unlock() }; return value }
    func set(_ newValue: Value) { lock.lock(); value = newValue; lock.unlock() }
    @discardableResult func update<T>(_ body: (inout Value) -> T) -> T { lock.lock(); defer { lock.unlock() }; return body(&value) }
}
