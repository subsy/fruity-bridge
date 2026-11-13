import Foundation

/// Checks that need only the request line and headers; they run before the body is read.
enum RequestGuard {
    static let localHosts: Set<String> = ["127.0.0.1:\(bridgePort)", "localhost:\(bridgePort)"]

    /// `host[:port]` of the tailnet URL, lowercased: the Host header `tailscale serve` forwards.
    static func authority(of url: String?) -> String? {
        guard let url, let components = URLComponents(string: url), let host = components.host?.lowercased(), !host.isEmpty else { return nil }
        return components.port.map { "\(host):\($0)" } ?? host
    }

    /// DNS-rebinding protection: browsers send Origin or Sec-Fetch-*, and a rebound name shows in Host.
    static func browserCheck(_ request: HTTPRequest, tailnetURL: String?) -> APIError? {
        if ["origin", "sec-fetch-site", "sec-fetch-mode"].contains(where: { request.headers[$0] != nil }) {
            return .forbidden("Requests from web browsers aren't accepted.")
        }
        var allowed = localHosts
        if let tailnet = authority(of: tailnetURL) { allowed.insert(tailnet) }
        guard let host = request.header("host")?.lowercased(), allowed.contains(host) else {
            return .forbidden("This bridge only answers on its tailnet name or 127.0.0.1:\(bridgePort).")
        }
        return nil
    }

    /// The path after the `/<secret>` prefix `tailscale serve` adds, or nil when it isn't there.
    static func stripSecret(_ rawPath: String, secret: String?) -> String? {
        guard let secret, !secret.isEmpty else { return nil }
        let prefix = Array("/\(secret)".utf8)
        let path = Array(rawPath.utf8)
        guard path.count >= prefix.count else { return nil }
        var difference: UInt8 = 0
        for i in prefix.indices { difference |= path[i] ^ prefix[i] }
        guard difference == 0 else { return nil }
        let rest = String(decoding: path[prefix.count...], as: UTF8.self)
        if rest.isEmpty { return "/" }
        return rest.hasPrefix("/") ? rest : nil
    }

    /// API path without a trailing slash.
    static func normalize(_ path: String) -> String {
        path.count > 1 && path.hasSuffix("/") ? String(path.dropLast()) : path
    }

    /// Browser/Host checks, then the tailnet identity. Only requests that carry the path secret
    /// (that is, came through `tailscale serve`) may use Tailscale-User-Login. `GET /v1/health`
    /// needs neither. On success `request.rawPath` no longer has the secret.
    static func authorize(_ request: inout HTTPRequest, config: BridgeConfig) -> APIError? {
        if let error = browserCheck(request, tailnetURL: config.tailnetURL) { return error }
        let viaServe = stripSecret(request.rawPath, secret: config.pathSecret)
        if let viaServe { request.rawPath = viaServe }
        if normalize(request.rawPath) == "/v1/health" { return nil }
        guard viaServe != nil else {
            let hint = config.pathSecret?.isEmpty == false ? "" : " The bridge has no path secret yet; run install.sh on the Mac."
            return .unauthorized("Requests must come through tailscale serve.\(hint)")
        }
        guard let login = request.header("tailscale-user-login")?.trimmingCharacters(in: .whitespaces), !login.isEmpty else {
            return .unauthorized("Requests must come through tailscale serve (no Tailscale-User-Login header).")
        }
        guard config.allows(login) else {
            return .forbidden("\(login) isn't allowed to use this bridge.")
        }
        return nil
    }
}

/// Routes API requests. Runs on HTTP connection threads.
final class Router {
    let notes: NotesStore
    let reminders: RemindersService?
    let config: Locked<BridgeConfig>
    /// Whether the Notes database may be touched (Full Disk Access present or the user approved it).
    let notesAllowed: () -> Bool
    /// Runs a Notes write on the main thread; nil when writes aren't available (CLI mode).
    let writeNotes: (((NotesWriter) throws -> Void) throws -> Void)?
    let notesWriteStatus: () -> String

    init(notes: NotesStore, reminders: RemindersService?, config: Locked<BridgeConfig>, notesAllowed: @escaping () -> Bool,
         writeNotes: (((NotesWriter) throws -> Void) throws -> Void)?, notesWriteStatus: @escaping () -> String) {
        self.notes = notes
        self.reminders = reminders
        self.config = config
        self.notesAllowed = notesAllowed
        self.writeNotes = writeNotes
        self.notesWriteStatus = notesWriteStatus
    }

    /// Logs requests refused by the browser/Host checks (menu-bar app and --serve-notes-only).
    var logRejections = false

    /// Pre-body checks (see RequestGuard.authorize); nil when the request may proceed.
    func authorize(_ request: inout HTTPRequest) -> HTTPResponse? {
        guard let error = RequestGuard.authorize(&request, config: config.get()) else { return nil }
        if logRejections, error.status == 403 {
            let host = String((request.header("host") ?? "none").unicodeScalars.prefix(120).map { $0.isASCII && $0.value >= 32 ? Character($0) : "?" })
            print("Refused \(request.method) request: \(error.message) (Host: \(host), Origin: \(request.header("origin") != nil ? "set" : "none"))")
        }
        return .error(error)
    }

    /// `authorize` then `dispatch`, for callers that already have the whole request.
    func handle(_ request: HTTPRequest) -> HTTPResponse {
        var request = request
        if let refusal = authorize(&request) { return refusal }
        return dispatch(request)
    }

    /// Routes an authorized request.
    func dispatch(_ request: HTTPRequest) -> HTTPResponse {
        do {
            return try route(request, path: RequestGuard.normalize(request.rawPath))
        } catch let error as APIError {
            return .error(error)
        } catch {
            return .error(.internalError("\(error)"))
        }
    }

    /// The decoded remainder of `path` after `prefix` (ids may contain encoded `/` and `:`).
    private func tail(_ path: String, _ prefix: String) -> String? {
        guard path.hasPrefix(prefix), path.count > prefix.count else { return nil }
        let raw = String(path.dropFirst(prefix.count))
        return raw.removingPercentEncoding ?? raw
    }

    private func requireNotes() throws {
        guard notesAllowed() else { throw APIError.permission(PermissionMessage.notesDatabase) }
    }

    /// The service checks its own authorization on every call; this only covers modes without it.
    private func requireReminders() throws -> RemindersService {
        guard let reminders else { throw APIError.permission(PermissionMessage.reminders) }
        return reminders
    }

    private func route(_ request: HTTPRequest, path: String) throws -> HTTPResponse {
        let method = request.method == "HEAD" ? "GET" : request.method
        func methodNotAllowed() -> HTTPResponse { .error(APIError(status: 405, code: "bad_request", message: "\(request.method) isn't supported here.")) }

        switch path {
        case "/v1/health":
            guard method == "GET" else { return methodNotAllowed() }
            return .json([
                "version": bridgeVersion,
                "reminders": reminders != nil ? reminders!.healthStatus : "missing",
                "notes": notesAllowed() ? notes.healthStatus() : "missing",
                "notesWrite": writeNotes != nil ? notesWriteStatus() : "missing",
            ])
        case "/v1/events":
            guard method == "GET" else { return methodNotAllowed() }
            // HEAD gets the stream's headers without opening a stream.
            return request.method == "HEAD" ? HTTPResponse(status: 200, contentType: "text/event-stream") : .eventStream
        case "/v1/reminders/lists":
            guard method == "GET" else { return methodNotAllowed() }
            let counts: Bool
            switch request.query["counts"] ?? "true" {
            case "true": counts = true
            case "false": counts = false
            default: throw APIError.badRequest("`counts` must be true or false.")
            }
            return .json(["lists": try requireReminders().lists(counts: counts)])
        case "/v1/reminders":
            switch method {
            case "GET": return .json(["reminders": try requireReminders().reminders(query: request.query)])
            case "POST": return .json(try requireReminders().create(try request.jsonBody()), status: 201)
            default: return methodNotAllowed()
            }
        case "/v1/notes/folders":
            guard method == "GET" else { return methodNotAllowed() }
            try requireNotes()
            return .json(["folders": try notes.folders()])
        case "/v1/notes":
            switch method {
            case "GET":
                try requireNotes()
                var recent: Int?
                if let value = request.query["recent"] {
                    guard let n = Int(value), n >= 0 else { throw APIError.badRequest("`recent` must be a number.") }
                    recent = n
                }
                return .json(["notes": try notes.notes(folderID: request.query["folder"], search: request.query["q"], recent: recent)])
            case "POST":
                return .json(try createNote(try request.jsonBody()), status: 201)
            default: return methodNotAllowed()
            }
        default: break
        }

        if let id = tail(path, "/v1/reminders/") {
            let service = try requireReminders()
            switch method {
            case "GET": return .json(try service.reminder(id: id))
            case "PATCH": return .json(try service.update(id: id, try request.jsonBody()))
            case "DELETE":
                try service.delete(id: id)
                return .noContent
            default: return methodNotAllowed()
            }
        }
        if let id = tail(path, "/v1/notes/attachments/") {
            guard method == "GET" else { return methodNotAllowed() }
            try requireNotes()
            let (url, type) = try notes.attachmentFile(id: id)
            return HTTPResponse(status: 200, contentType: type, fileURL: url)
        }
        if let id = tail(path, "/v1/notes/") {
            switch method {
            case "GET":
                try requireNotes()
                return .json(try notes.entry(id: id).fullJSON)
            case "PUT": return .json(try updateNote(id: id, try request.jsonBody()))
            case "DELETE":
                try deleteNote(id: id)
                return .noContent
            default: return methodNotAllowed()
            }
        }
        return .error(.notFound("No such endpoint."))
    }

    // MARK: Notes writes

    private func writer() throws -> ((NotesWriter) throws -> Void) throws -> Void {
        guard let writeNotes else {
            throw APIError.permission(PermissionMessage.notesWritesUnavailable)
        }
        return writeNotes
    }

    private func markdownHTML(_ body: [String: Any]) throws -> String {
        guard let markdown = body["markdown"] as? String else { throw APIError.badRequest("`markdown` is required.") }
        let blocks = NoteMarkdown.parse(markdown)
        guard !blocks.isEmpty else { throw APIError.badRequest("The note is empty.") }
        return NoteMarkdown.html(blocks)
    }

    /// Re-reads a note after a write until the store reflects it (up to 3 s). nil when it didn't
    /// catch up: the note isn't there yet (create) or its `modifiedAt` hasn't moved (update).
    private func reread(id: String, changedFrom previous: Date?) -> NoteEntry? {
        let deadline = Date().addingTimeInterval(3)
        repeat {
            if let entry = try? notes.entry(id: id), previous == nil || entry.modifiedAt != previous { return entry }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return nil
    }

    /// What a successful write returns when the database hasn't caught up: never an error, so
    /// clients don't retry a write that happened.
    private static func pendingNote(id: String) -> [String: Any] { ["id": id, "modifiedAt": NSNull()] }

    private func createNote(_ body: [String: Any]) throws -> [String: Any] {
        guard let folderID = body["folderId"] as? String else { throw APIError.badRequest("`folderId` is required.") }
        let html = try markdownHTML(body)
        try requireNotes()
        let write = try writer()
        let folder = try notes.folder(id: folderID)
        guard !folder.isTrash, !folder.isSmart else { throw APIError.notEditable("New notes can't go in Recently Deleted or a smart folder.") }
        var newID = ""
        try write { newID = try $0.makeNote(folderID: folder.id, html: html) }
        return reread(id: newID, changedFrom: nil)?.fullJSON ?? Router.pendingNote(id: newID)
    }

    private func updateNote(id: String, _ body: [String: Any]) throws -> [String: Any] {
        guard let expectedText = body["expectedModifiedAt"] as? String else {
            throw APIError.badRequest("`expectedModifiedAt` is required (the note's `modifiedAt` when editing started).")
        }
        guard let expected = RFC3339.parse(expectedText) else { throw APIError.badRequest("`expectedModifiedAt` must be an RFC 3339 timestamp.") }
        let html = try markdownHTML(body)
        try requireNotes()
        let write = try writer()
        let entry = try notes.entry(id: id)
        guard RFC3339.sameSecond(expected, entry.modifiedAt) else {
            throw APIError.conflict("This note changed on another device.", current: entry.fullJSON)
        }
        guard entry.editable else {
            let reason = entry.locked ? "it's locked" : entry.inTrash ? "it's in Recently Deleted"
                : entry.decoded.hasInlineAttachments ? "it has tags, mentions or note links" : "it has attachments, a table or a checklist"
            throw APIError.notEditable("This note can't be edited here because \(reason). Edit it on a Mac or iPhone.")
        }
        try write { try $0.setBody(noteID: entry.id, html: html) }
        return reread(id: entry.id, changedFrom: entry.modifiedAt)?.fullJSON ?? Router.pendingNote(id: entry.id)
    }

    private func deleteNote(id: String) throws {
        try requireNotes()
        let write = try writer()
        let entry = try notes.entry(id: id)
        // Deleting from Recently Deleted would erase the note for good; leave that to Notes itself.
        guard !entry.inTrash else { throw APIError.notEditable("This note is already in Recently Deleted.") }
        let folders = try notes.deletionFolders(noteID: entry.id)
        try write { try $0.delete(noteID: entry.id, folderID: folders.source, trashFolderID: folders.trash) }
        // AppleScript can acknowledge a delete without changing Notes. Wait on this HTTP
        // thread, outside writeNotes, so Notes' main thread remains free to save the change.
        let deadline = ProcessInfo.processInfo.systemUptime + 3
        while true {
            do {
                if try notes.entry(id: entry.id).inTrash { return }
            } catch let error as APIError where error.code == "not_found" {
                return
            }
            let remaining = deadline - ProcessInfo.processInfo.systemUptime
            guard remaining > 0 else {
                throw APIError.internalError("Notes didn't confirm the move to Recently Deleted within 3 s. Check the note in Notes before retrying.")
            }
            Thread.sleep(forTimeInterval: min(0.25, remaining))
        }
    }
}
