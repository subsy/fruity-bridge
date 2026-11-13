import Foundation
import SQLite3
import UniformTypeIdentifiers

// MARK: - SQLite

enum SQLValue {
    case null, int(Int64), double(Double), text(String), blob([UInt8])
    var int: Int64? {
        switch self {
        case .int(let v): return v
        case .double(let v): return Int64(v)
        default: return nil
        }
    }
    var double: Double? {
        switch self {
        case .double(let v): return v
        case .int(let v): return Double(v)
        default: return nil
        }
    }
    var string: String? { if case .text(let v) = self { return v }; return nil }
    var blob: [UInt8]? {
        switch self {
        case .blob(let v): return v
        case .text(let v): return Array(v.utf8)
        default: return nil
        }
    }
    var bool: Bool { (int ?? 0) != 0 }
}

typealias SQLRow = [String: SQLValue]

struct SQLiteError: Error, CustomStringConvertible {
    var code: Int32
    var message: String
    var description: String { "SQLite \(code): \(message)" }
}

/// A read-only connection. Opening read-only (not immutable) keeps WAL readers consistent with
/// the live Notes app without copying the database.
final class SQLiteDB {
    private let handle: OpaquePointer

    init(readOnly path: String) throws {
        var raw: OpaquePointer?
        let rc = sqlite3_open_v2(path, &raw, SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX, nil)
        guard rc == SQLITE_OK, let raw else {
            let message = raw.map { String(cString: sqlite3_errmsg($0)) } ?? "open failed"
            if let raw { sqlite3_close(raw) }
            throw SQLiteError(code: rc, message: message)
        }
        handle = raw
        sqlite3_busy_timeout(raw, 3000)
    }

    deinit { sqlite3_close(handle) }

    func query(_ sql: String, _ params: [SQLValue] = []) throws -> [SQLRow] {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK, let statement else {
            throw SQLiteError(code: sqlite3_errcode(handle), message: String(cString: sqlite3_errmsg(handle)))
        }
        defer { sqlite3_finalize(statement) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        for (index, value) in params.enumerated() {
            let position = Int32(index + 1)
            switch value {
            case .null: sqlite3_bind_null(statement, position)
            case .int(let v): sqlite3_bind_int64(statement, position, v)
            case .double(let v): sqlite3_bind_double(statement, position, v)
            case .text(let v): sqlite3_bind_text(statement, position, v, -1, transient)
            case .blob(let v): v.withUnsafeBytes { _ = sqlite3_bind_blob(statement, position, $0.baseAddress, Int32($0.count), transient) }
            }
        }
        let count = sqlite3_column_count(statement)
        let names = (0..<count).map { String(cString: sqlite3_column_name(statement, $0)) }
        var rows: [SQLRow] = []
        while true {
            let rc = sqlite3_step(statement)
            if rc == SQLITE_DONE { break }
            guard rc == SQLITE_ROW else {
                throw SQLiteError(code: rc, message: String(cString: sqlite3_errmsg(handle)))
            }
            var row = SQLRow()
            for i in 0..<count {
                switch sqlite3_column_type(statement, i) {
                case SQLITE_INTEGER: row[names[Int(i)]] = .int(sqlite3_column_int64(statement, i))
                case SQLITE_FLOAT: row[names[Int(i)]] = .double(sqlite3_column_double(statement, i))
                case SQLITE_TEXT: row[names[Int(i)]] = .text(String(cString: sqlite3_column_text(statement, i)))
                case SQLITE_BLOB:
                    let size = Int(sqlite3_column_bytes(statement, i))
                    if let pointer = sqlite3_column_blob(statement, i), size > 0 {
                        row[names[Int(i)]] = .blob(Array(UnsafeRawBufferPointer(start: pointer, count: size)))
                    } else {
                        row[names[Int(i)]] = .blob([])
                    }
                default: row[names[Int(i)]] = .null
                }
            }
            rows.append(row)
        }
        return rows
    }
}

// MARK: - Notes IDs

/// AppleScript-compatible Core Data object IDs: `x-coredata://<Z_METADATA.Z_UUID>/ICNote/p<Z_PK>`.
enum NotesID {
    static func make(store: String, entity: String, pk: Int64) -> String { "x-coredata://\(store)/\(entity)/p\(pk)" }

    static func parse(_ id: String, entity: String, store: String) -> Int64? {
        var text = id
        // Some proxies collapse `//`; accept `x-coredata:/UUID/...` too.
        if text.hasPrefix("x-coredata:/"), !text.hasPrefix("x-coredata://") { text = "x-coredata://" + text.dropFirst("x-coredata:/".count) }
        guard text.hasPrefix("x-coredata://") else { return nil }
        let parts = text.dropFirst("x-coredata://".count).split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0].caseInsensitiveCompare(store) == .orderedSame, parts[1] == entity,
              parts[2].hasPrefix("p"), let pk = Int64(parts[2].dropFirst()), pk > 0 else { return nil }
        return pk
    }
}

// MARK: - Store

struct NoteEntry {
    var pk: Int64
    var id: String
    var folderPK: Int64
    var folderID: String
    var title: String
    var snippet: String?
    var createdAt: Date?
    var modifiedAt: Date?
    var pinned: Bool
    var locked: Bool
    var inTrash: Bool
    var decoded: DecodedNote

    var editable: Bool {
        !locked && !inTrash && !decoded.hasAttachments && !decoded.hasChecklist && !decoded.hasInlineAttachments
            && !decoded.hasQuote
    }

    var summaryJSON: [String: Any] {
        [
            "id": id, "folderId": folderID, "title": title, "snippet": snippet ?? NSNull(),
            "createdAt": RFC3339.string(createdAt), "modifiedAt": RFC3339.string(modifiedAt),
            "pinned": pinned, "locked": locked, "editable": editable,
            "hasAttachments": decoded.hasAttachments, "hasChecklist": decoded.hasChecklist,
        ]
    }

    var fullJSON: [String: Any] {
        var out = summaryJSON
        out["blocks"] = locked ? [] : decoded.blocks.map(\.json)
        if editable { out["markdown"] = NoteMarkdown.render(decoded.blocks) }
        return out
    }
}

struct FolderEntry {
    var pk: Int64
    var id: String
    var name: String
    var account: String?
    var parentPK: Int64?
    var isTrash: Bool
    var isSmart: Bool
    var smartQuery: String?
    var identifier: String?
}

/// Thread-safe: every database access goes through `withDB`, which holds a lock.
final class NotesStore: @unchecked Sendable {
    static var defaultContainer: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Group Containers/group.com.apple.notes", isDirectory: true)
    }

    let container: URL
    private let lock = NSRecursiveLock()
    private var db: SQLiteDB?
    private var schema: Schema?
    private var cache: [Int64: (key: String, note: DecodedNote)] = [:]

    private struct Schema {
        var store: String
        var entities: [String: Int64]
        var columns: Set<String>
        func ent(_ name: String) -> Int64 { entities[name] ?? -1 }
        func col(_ alias: String, _ names: String...) -> String {
            let present = names.filter { columns.contains($0) }.map { "\(alias).\($0)" }
            if present.isEmpty { return "NULL" }
            return present.count == 1 ? present[0] : "coalesce(\(present.joined(separator: ", ")))"
        }
    }

    init(container: URL = NotesStore.defaultContainer) { self.container = container }

    var databasePath: String { container.appendingPathComponent("NoteStore.sqlite").path }

    private func withDB<T>(_ body: (SQLiteDB, Schema) throws -> T) throws -> T {
        lock.lock()
        defer { lock.unlock() }
        do {
            if db == nil || schema == nil {
                let connection = try SQLiteDB(readOnly: databasePath)
                let store = try connection.query("SELECT Z_UUID FROM Z_METADATA LIMIT 1").first?["Z_UUID"]?.string
                guard let store else { throw BridgeError("Notes database has no store UUID") }
                var entities: [String: Int64] = [:]
                for row in try connection.query("SELECT Z_ENT, Z_NAME FROM Z_PRIMARYKEY") {
                    if let name = row["Z_NAME"]?.string, let ent = row["Z_ENT"]?.int { entities[name] = ent }
                }
                let columns = Set(try connection.query("PRAGMA table_info(ZICCLOUDSYNCINGOBJECT)").compactMap { $0["name"]?.string })
                db = connection
                schema = Schema(store: store, entities: entities, columns: columns)
            }
            return try body(db!, schema!)
        } catch let error as SQLiteError {
            db = nil
            schema = nil
            if [SQLITE_CANTOPEN, SQLITE_AUTH, SQLITE_PERM].contains(error.code) || error.message.contains("authorization denied") {
                throw APIError.permission(PermissionMessage.notesDatabase)
            }
            throw APIError.internalError("Notes database error: \(error.message)")
        } catch let error as BridgeError {
            db = nil
            schema = nil
            throw APIError.internalError(error.description)
        }
    }

    /// `ok`, `missing` (no access) or `error`.
    func healthStatus() -> String {
        do {
            _ = try withDB { db, _ in try db.query("SELECT count(*) FROM Z_METADATA") }
            return "ok"
        } catch let error as APIError {
            return error.code == "permission_missing" ? "missing" : "error"
        } catch {
            return "error"
        }
    }

    /// Changes whenever any note or folder is saved (Z_OPT increments on every Core Data save).
    func changeSignature() throws -> String {
        try withDB { db, schema in
            let row = try db.query(
                "SELECT count(*) c, max(\(schema.col("o", "ZMODIFICATIONDATE1", "ZMODIFICATIONDATE"))) m, sum(o.Z_OPT) s FROM ZICCLOUDSYNCINGOBJECT o WHERE o.Z_ENT IN (?, ?, ?)",
                [.int(schema.ent("ICNote")), .int(schema.ent("ICFolder")), .int(schema.ent("ICAttachment"))]).first ?? [:]
            return "\(row["c"]?.int ?? 0)/\(row["m"]?.double ?? 0)/\(row["s"]?.int ?? 0)"
        }
    }

    // MARK: Folders

    private func loadFolders(_ db: SQLiteDB, _ schema: Schema) throws -> [FolderEntry] {
        let sql = """
            SELECT f.Z_PK pk, \(schema.col("f", "ZTITLE2")) name, \(schema.col("f", "ZFOLDERTYPE")) type, \(schema.col("f", "ZPARENT")) parent,
                   \(schema.col("f", "ZSMARTFOLDERQUERYJSON")) query, \(schema.col("f", "ZISHIDDENNOTECONTAINER")) hidden,
                   f.ZIDENTIFIER ident, \(schema.col("a", "ZNAME")) account
            FROM ZICCLOUDSYNCINGOBJECT f LEFT JOIN ZICCLOUDSYNCINGOBJECT a ON a.Z_PK = \(schema.col("f", "ZOWNER"))
            WHERE f.Z_ENT = ? AND coalesce(f.ZMARKEDFORDELETION, 0) = 0
            """
        return try db.query(sql, [.int(schema.ent("ICFolder"))]).compactMap { row in
            guard let pk = row["pk"]?.int else { return nil }
            let type = row["type"]?.int ?? 0
            if row["hidden"]?.bool == true && type != 1 { return nil }
            return FolderEntry(
                pk: pk, id: NotesID.make(store: schema.store, entity: "ICFolder", pk: pk),
                name: row["name"]?.string ?? (type == 1 ? "Recently Deleted" : "Folder"), account: row["account"]?.string,
                parentPK: row["parent"]?.int, isTrash: type == 1, isSmart: type == 2 || row["query"]?.string != nil,
                smartQuery: row["query"]?.string, identifier: row["ident"]?.string)
        }
    }

    /// Sidebar order: per account, Quick Notes, the default Notes folder, the rest by name with
    /// children after their parent, Recently Deleted last.
    private func orderFolders(_ folders: [FolderEntry]) -> [FolderEntry] {
        func rank(_ f: FolderEntry) -> Int {
            if f.isTrash { return 3 }
            if f.identifier?.hasPrefix("SystemPaper") == true { return 0 }
            if f.identifier?.hasPrefix("DefaultFolder") == true { return 1 }
            return 2
        }
        func less(_ a: FolderEntry, _ b: FolderEntry) -> Bool {
            if rank(a) != rank(b) { return rank(a) < rank(b) }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
        let known = Set(folders.map(\.pk))
        var children: [Int64: [FolderEntry]] = [:]
        var roots: [FolderEntry] = []
        for folder in folders {
            if let parent = folder.parentPK, known.contains(parent) { children[parent, default: []].append(folder) } else { roots.append(folder) }
        }
        var out: [FolderEntry] = []
        func visit(_ folder: FolderEntry, depth: Int) {
            out.append(folder)
            guard depth < 32 else { return }
            for child in (children[folder.pk] ?? []).sorted(by: less) { visit(child, depth: depth + 1) }
        }
        let accounts = Dictionary(grouping: roots, by: { $0.account ?? "" })
        for account in accounts.keys.sorted(by: { $0.localizedStandardCompare($1) == .orderedAscending }) {
            for root in accounts[account]!.sorted(by: less) { visit(root, depth: 0) }
        }
        return out
    }

    func folders() throws -> [[String: Any]] {
        try withDB { db, schema in
            let folders = orderFolders(try loadFolders(db, schema))
            let notes = try loadNotes(db, schema, folders: folders)
            var counts: [Int64: Int] = [:]
            for note in notes { counts[note.folderPK, default: 0] += 1 }
            let tags = try noteTags(db, schema)
            let ids = Dictionary(uniqueKeysWithValues: folders.map { ($0.pk, $0.id) })
            return folders.map { folder in
                var count: Any = counts[folder.pk] ?? 0
                if folder.isSmart {
                    count = (try? smartMatches(folder, notes: notes, tags: tags).count) ?? NSNull()
                }
                return [
                    "id": folder.id, "name": folder.name, "account": folder.account ?? NSNull(),
                    "parentId": folder.parentPK.flatMap { ids[$0] } ?? NSNull(), "noteCount": count,
                    "isTrash": folder.isTrash, "isSmart": folder.isSmart,
                ] as [String: Any]
            }
        }
    }

    // MARK: Smart folders

    private struct UnsupportedQuery: Error {}

    private func noteTags(_ db: SQLiteDB, _ schema: Schema) throws -> [Int64: Set<String>] {
        var tags: [Int64: Set<String>] = [:]
        let rows = try db.query("""
            SELECT \(schema.col("i", "ZNOTE1", "ZNOTE")) note, \(schema.col("i", "ZTOKENCONTENTIDENTIFIER")) tag FROM ZICCLOUDSYNCINGOBJECT i
            WHERE i.Z_ENT = ? AND \(schema.col("i", "ZTYPEUTI1")) = 'com.apple.notes.inlinetextattachment.hashtag' AND coalesce(i.ZMARKEDFORDELETION, 0) = 0
            """, [.int(schema.ent("ICInlineAttachment"))])
        for row in rows {
            if let note = row["note"]?.int, let tag = row["tag"]?.string { tags[note, default: []].insert(tag.uppercased()) }
        }
        return tags
    }

    /// Evaluates the subset of smart-folder queries Notes writes for tag folders:
    /// `and`/`or` combinations of `deleted` and `tag`. Anything else is unsupported.
    private func smartMatches(_ folder: FolderEntry, notes: [NoteEntry], tags: [Int64: Set<String>]) throws -> [NoteEntry] {
        guard let text = folder.smartQuery, let data = text.data(using: .utf8),
              let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any], let predicate = root["type"] else {
            throw UnsupportedQuery()
        }
        if let entity = root["entity"] as? String, entity != "note" { throw UnsupportedQuery() }
        func evaluate(_ predicate: Any, _ note: NoteEntry) throws -> Bool {
            guard let object = predicate as? [String: Any], object.count == 1, let (key, value) = object.first else { throw UnsupportedQuery() }
            switch key {
            case "and":
                guard let items = value as? [Any] else { throw UnsupportedQuery() }
                for item in items where try !evaluate(item, note) { return false }
                return true
            case "or":
                guard let items = value as? [Any] else { throw UnsupportedQuery() }
                for item in items where try evaluate(item, note) { return true }
                return false
            case "deleted":
                guard let flag = value as? Bool else { throw UnsupportedQuery() }
                return note.inTrash == flag
            case "tag":
                guard let tag = value as? String else { throw UnsupportedQuery() }
                return tags[note.pk]?.contains(tag.uppercased()) ?? false
            default:
                throw UnsupportedQuery()
            }
        }
        return try notes.filter { try evaluate(predicate, $0) }
    }

    // MARK: Notes

    private func loadNotes(_ db: SQLiteDB, _ schema: Schema, folders: [FolderEntry], onlyPK: Int64? = nil) throws -> [NoteEntry] {
        let folderMap = Dictionary(uniqueKeysWithValues: folders.map { ($0.pk, $0) })
        // Notes shows Quick Notes (system paper) only in the Quick Notes folder, though they're
        // stored in the account's default folder.
        var quickNotesFolders: [String: FolderEntry] = [:]
        for folder in folders where folder.identifier?.hasPrefix("SystemPaper") == true { quickNotesFolders[folder.account ?? ""] = folder }
        var sql = """
            SELECT n.Z_PK pk, n.Z_OPT opt, coalesce(d.Z_OPT, 0) dopt, n.ZNOTEDATA notedata, \(schema.col("n", "ZTITLE1")) title,
                   \(schema.col("n", "ZSNIPPET")) snippet, \(schema.col("n", "ZCREATIONDATE3", "ZCREATIONDATE1")) created,
                   \(schema.col("n", "ZMODIFICATIONDATE1")) modified, \(schema.col("n", "ZISPINNED")) pinned,
                   \(schema.col("n", "ZISPASSWORDPROTECTED")) locked, \(schema.col("n", "ZISSYSTEMPAPER")) quicknote, n.ZFOLDER folder
            FROM ZICCLOUDSYNCINGOBJECT n LEFT JOIN ZICNOTEDATA d ON d.Z_PK = n.ZNOTEDATA
            WHERE n.Z_ENT = ? AND n.ZFOLDER IS NOT NULL AND coalesce(n.ZMARKEDFORDELETION, 0) = 0
              AND \(schema.col("n", "ZTITLE1")) IS NOT NULL
            """
        var params: [SQLValue] = [.int(schema.ent("ICNote"))]
        if let onlyPK {
            sql += " AND n.Z_PK = ?"
            params.append(.int(onlyPK))
        }
        var entries: [NoteEntry] = []
        for row in try db.query(sql, params) {
            guard let pk = row["pk"]?.int, let storedFolderPK = row["folder"]?.int, var folder = folderMap[storedFolderPK], !folder.isSmart else { continue }
            if row["quicknote"]?.bool == true, !folder.isTrash, let quick = quickNotesFolders[folder.account ?? ""] { folder = quick }
            let folderPK = folder.pk
            let locked = row["locked"]?.bool ?? false
            let key = "\(row["opt"]?.int ?? 0)/\(row["dopt"]?.int ?? 0)/\(row["modified"]?.double ?? 0)"
            let decoded: DecodedNote
            if locked {
                decoded = DecodedNote(blocks: [], plainText: "", hasAttachments: false, hasChecklist: false)
            } else if let cached = cache[pk], cached.key == key {
                decoded = cached.note
            } else {
                decoded = decodeNote(db, schema, noteData: row["notedata"]?.int)
                cache[pk] = (key, decoded)
            }
            var title = row["title"]?.string ?? ""
            if title.isEmpty { title = decoded.plainText.split(separator: "\n").first.map(String.init) ?? "New Note" }
            var snippet = row["snippet"]?.string
            if snippet == nil || snippet?.isEmpty == true, !locked {
                let lines = decoded.plainText.split(separator: "\n").dropFirst()
                snippet = lines.first.map { String($0.prefix(120)) }
            }
            entries.append(NoteEntry(
                pk: pk, id: NotesID.make(store: schema.store, entity: "ICNote", pk: pk), folderPK: folderPK, folderID: folder.id,
                title: title, snippet: snippet,
                createdAt: row["created"]?.double.map { Date(timeIntervalSinceReferenceDate: $0) },
                modifiedAt: row["modified"]?.double.map { Date(timeIntervalSinceReferenceDate: $0) },
                pinned: row["pinned"]?.bool ?? false, locked: locked, inTrash: folder.isTrash, decoded: decoded))
        }
        return entries
    }

    static func sortSummaries(_ notes: [NoteEntry]) -> [NoteEntry] {
        notes.sorted { a, b in
            if a.pinned != b.pinned { return a.pinned }
            let ma = a.modifiedAt ?? .distantPast, mb = b.modifiedAt ?? .distantPast
            if ma != mb { return ma > mb }
            return a.pk > b.pk
        }
    }

    func notes(folderID: String?, search: String?, recent: Int?) throws -> [[String: Any]] {
        try withDB { db, schema in
            let folders = try loadFolders(db, schema)
            var notes = try loadNotes(db, schema, folders: folders)
            if let folderID {
                guard let pk = NotesID.parse(folderID, entity: "ICFolder", store: schema.store), let folder = folders.first(where: { $0.pk == pk }) else {
                    throw APIError.notFound("Folder not found.")
                }
                if folder.isSmart {
                    do {
                        notes = try smartMatches(folder, notes: notes, tags: try noteTags(db, schema))
                    } catch is UnsupportedQuery {
                        throw APIError(status: 422, code: "bad_request", message: "This smart folder's filter isn't supported here; only tag-based smart folders are.")
                    }
                } else {
                    notes = notes.filter { $0.folderPK == pk }
                }
            } else {
                notes = notes.filter { !$0.inTrash }
            }
            if let search = search?.trimmingCharacters(in: .whitespacesAndNewlines), !search.isEmpty {
                let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
                notes = notes.filter { $0.title.range(of: search, options: options) != nil || $0.decoded.plainText.range(of: search, options: options) != nil }
            }
            if let recent {
                notes = Array(notes.sorted { ($0.modifiedAt ?? .distantPast) > ($1.modifiedAt ?? .distantPast) }.prefix(max(0, recent)))
            }
            return NotesStore.sortSummaries(notes).map(\.summaryJSON)
        }
    }

    func entry(id: String) throws -> NoteEntry {
        try withDB { db, schema in
            guard let pk = NotesID.parse(id, entity: "ICNote", store: schema.store),
                  let note = try loadNotes(db, schema, folders: try loadFolders(db, schema), onlyPK: pk).first else {
                throw APIError.notFound("Note not found.")
            }
            return note
        }
    }

    func folder(id: String) throws -> FolderEntry {
        try withDB { db, schema in
            guard let pk = NotesID.parse(id, entity: "ICFolder", store: schema.store),
                  let folder = try loadFolders(db, schema).first(where: { $0.pk == pk }) else {
                throw APIError.notFound("Folder not found.")
            }
            return folder
        }
    }

    // MARK: Decoding

    private func decodeNote(_ db: SQLiteDB, _ schema: Schema, noteData: Int64?) -> DecodedNote {
        let empty = DecodedNote(blocks: [], plainText: "", hasAttachments: false, hasChecklist: false)
        guard let noteData, let blob = try? db.query("SELECT ZDATA FROM ZICNOTEDATA WHERE Z_PK = ?", [.int(noteData)]).first?["ZDATA"]?.blob,
              !blob.isEmpty, let bytes = try? Gzip.inflate(blob), let document = try? NoteDocument.decode(noteStoreProto: bytes) else {
            return empty
        }
        let ids = Set(document.runs.compactMap(\.attachmentID))
        let rows = (try? attachmentRows(db, schema, identifiers: Array(ids))) ?? [:]
        return NoteBlockBuilder.build(document) { id, uti in resolve(db, schema, id: id, uti: uti, rows: rows) }
    }

    private func attachmentRows(_ db: SQLiteDB, _ schema: Schema, identifiers: [String]) throws -> [String: SQLRow] {
        guard !identifiers.isEmpty else { return [:] }
        var out: [String: SQLRow] = [:]
        for chunk in stride(from: 0, to: identifiers.count, by: 200).map({ Array(identifiers[$0..<min($0 + 200, identifiers.count)]) }) {
            let sql = """
                SELECT a.Z_PK pk, a.Z_ENT ent, a.ZIDENTIFIER ident, \(schema.col("a", "ZTYPEUTI")) uti, \(schema.col("a", "ZTYPEUTI1")) uti1,
                       \(schema.col("a", "ZALTTEXT")) alt, \(schema.col("a", "ZTOKENCONTENTIDENTIFIER")) token, \(schema.col("a", "ZTITLE")) title,
                       \(schema.col("a", "ZUSERTITLE")) usertitle, \(schema.col("a", "ZURLSTRING")) urlstring,
                       \(schema.col("a", "ZISPASSWORDPROTECTED")) locked, \(schema.col("m", "ZFILENAME")) filename,
                       \(schema.col("a", "ZMODIFICATIONDATE")) modified
                FROM ZICCLOUDSYNCINGOBJECT a LEFT JOIN ZICCLOUDSYNCINGOBJECT m ON m.Z_PK = \(schema.col("a", "ZMEDIA"))
                WHERE a.ZIDENTIFIER IN (\(Array(repeating: "?", count: chunk.count).joined(separator: ",")))
                """
            for row in try db.query(sql, chunk.map { .text($0) }) {
                if let ident = row["ident"]?.string { out[ident] = row }
            }
        }
        return out
    }

    static func kind(uti: String?, hasMedia: Bool) -> String {
        guard let uti else { return "other" }
        switch uti {
        case "com.apple.notes.table": return "table"
        case "public.url": return "link"
        case "com.apple.drawing", "com.apple.drawing.2", "com.apple.paper": return "drawing"
        case "com.adobe.pdf", "com.apple.paper.doc.scan", "com.apple.paper.doc.pdf": return "pdf"
        case "com.apple.notes.gallery": return "image"
        default: break
        }
        if let type = UTType(uti) {
            if type.conforms(to: .image) { return "image" }
            if type.conforms(to: .pdf) { return "pdf" }
            if type.conforms(to: .audio) { return "audio" }
            if type.conforms(to: .movie) || type.conforms(to: .audiovisualContent) { return "video" }
        }
        return hasMedia ? "file" : "other"
    }

    private func resolve(_ db: SQLiteDB, _ schema: Schema, id: String, uti: String?, rows: [String: SQLRow]) -> ResolvedAttachment {
        let row = rows[id]
        if uti?.hasPrefix(NoteBlockBuilder.inlineUTIPrefix) == true || row?["ent"]?.int == schema.ent("ICInlineAttachment") {
            guard let text = row?["alt"]?.string, !text.isEmpty else { return .omit }
            var link: String?
            if let token = row?["token"]?.string, row?["uti1"]?.string?.hasSuffix(".link") == true, token.contains(":") { link = token }
            return .inline(text: text, link: link)
        }
        let effectiveUTI = row?["uti"]?.string ?? uti
        if effectiveUTI == "com.apple.notes.table", let pk = row?["pk"]?.int,
           let blob = try? db.query("SELECT ZMERGEABLEDATA1 d FROM ZICCLOUDSYNCINGOBJECT WHERE Z_PK = ?", [.int(pk)]).first?["d"]?.blob,
           let bytes = try? Gzip.inflate(blob),
           let rows = try? NoteTableDecoder.rows(mergeableData: bytes, cellText: { cell in
               NoteBlockBuilder.build(cell) { id, uti in
                   uti?.hasPrefix(NoteBlockBuilder.inlineUTIPrefix) == true
                       ? (((try? self.attachmentRows(db, schema, identifiers: [id]))?[id]?["alt"]?.string).map { .inline(text: $0, link: nil) } ?? .omit)
                       : .omit
               }.plainText
           }) {
            return .block(Block(kind: .table, rows: rows))
        }
        let hasMedia = row?["filename"]?.string != nil
        var ref = AttachmentRef(id: id, kind: NotesStore.kind(uti: effectiveUTI, hasMedia: hasMedia), uti: effectiveUTI)
        ref.filename = row?["filename"]?.string ?? row?["usertitle"]?.string
        ref.modifiedAt = row?["modified"]?.double.map { Date(timeIntervalSinceReferenceDate: $0) }
        if ref.kind == "link" {
            ref.href = row?["urlstring"]?.string
            ref.title = row?["title"]?.string
        }
        if let row, (try? fileFor(db, schema, attachment: row)) != nil {
            ref.url = "/v1/notes/attachments/" + percentEncode(id)
        }
        if effectiveUTI == "com.apple.notes.gallery", let pk = row?["pk"]?.int {
            let pages = (try? galleryChildren(db, schema, galleryPK: pk)) ?? []
            if !pages.isEmpty { ref.pages = pages.map { "/v1/notes/attachments/" + percentEncode($0) } }
        }
        return .block(Block(kind: .attachment, attachment: ref))
    }

    // MARK: Attachment files

    private func accountDirectories() -> [URL] {
        let accounts = container.appendingPathComponent("Accounts", isDirectory: true)
        let children = (try? FileManager.default.contentsOfDirectory(at: accounts, includingPropertiesForKeys: nil)) ?? []
        return children + [container]
    }

    private func safeComponent(_ value: String?) -> String? {
        guard let value, !value.isEmpty, value != ".", value != "..", !value.contains("/") else { return nil }
        return value
    }

    private func firstExisting(_ relativePaths: [[String?]]) -> URL? {
        for base in accountDirectories() {
            for components in relativePaths {
                let parts = components.map(safeComponent)
                guard !parts.contains(where: { $0 == nil }) else { continue }
                let url = parts.reduce(base) { $0.appendingPathComponent($1!) }
                var isDirectory: ObjCBool = false
                if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), !isDirectory.boolValue { return url }
            }
        }
        return nil
    }

    private func fileFor(_ db: SQLiteDB, _ schema: Schema, attachment row: SQLRow, depth: Int = 0) throws -> URL? {
        guard row["locked"]?.bool != true, let pk = row["pk"]?.int, let ident = row["ident"]?.string else { return nil }
        let uti = row["uti"]?.string
        let sql = """
            SELECT m.ZIDENTIFIER mid, \(schema.col("m", "ZGENERATION1", "ZGENERATION")) mgen, \(schema.col("m", "ZFILENAME")) mfile,
                   \(schema.col("a", "ZFALLBACKIMAGEGENERATION")) fimg, \(schema.col("a", "ZFALLBACKPDFGENERATION")) fpdf
            FROM ZICCLOUDSYNCINGOBJECT a LEFT JOIN ZICCLOUDSYNCINGOBJECT m ON m.Z_PK = \(schema.col("a", "ZMEDIA"))
            WHERE a.Z_PK = ?
            """
        guard let info = try db.query(sql, [.int(pk)]).first else { return nil }
        if let mid = info["mid"]?.string, let file = info["mfile"]?.string {
            if let url = firstExisting([["Media", mid, info["mgen"]?.string, file], ["Media", mid, file]]) { return url }
        }
        if let generation = info["fimg"]?.string {
            if let url = firstExisting([["FallbackImages", ident, generation, "FallbackImage.png"], ["FallbackImages", ident, generation, "FallbackImage.jpg"]]) { return url }
        }
        if let url = firstExisting([["FallbackImages", ident + ".jpg"], ["FallbackImages", ident + ".png"]]) { return url }
        if let generation = info["fpdf"]?.string, let url = firstExisting([["FallbackPDFs", ident, generation, "FallbackPDF.pdf"]]) { return url }
        if uti == "com.apple.notes.gallery", depth == 0, let first = try galleryChildren(db, schema, galleryPK: pk).first,
           let child = try attachmentRows(db, schema, identifiers: [first])[first] {
            return try fileFor(db, schema, attachment: child, depth: 1)
        }
        return nil
    }

    /// Scan pages in gallery order (apple_cloud_notes_parser's AppleNotesEmbeddedGallery heuristic),
    /// falling back to database order.
    private func galleryChildren(_ db: SQLiteDB, _ schema: Schema, galleryPK: Int64) throws -> [String] {
        let children = try db.query(
            "SELECT g.ZIDENTIFIER ident FROM ZICCLOUDSYNCINGOBJECT g WHERE \(schema.col("g", "ZPARENTATTACHMENT")) = ? AND g.Z_ENT = ? AND coalesce(g.ZMARKEDFORDELETION, 0) = 0 ORDER BY g.Z_PK",
            [.int(galleryPK), .int(schema.ent("ICAttachment"))]).compactMap { $0["ident"]?.string }
        guard children.count > 1, let blob = try db.query("SELECT \(schema.col("g", "ZMERGEABLEDATA1")) d FROM ZICCLOUDSYNCINGOBJECT g WHERE g.Z_PK = ?", [.int(galleryPK)]).first?["d"]?.blob,
              let bytes = try? Gzip.inflate(blob), let root = try? ProtoMessage(bytes),
              let entries = root.message(2)?.message(3)?.messages(3) else { return children }
        var order: [(Int64, String)] = []
        var uuid: String?
        var position: Int64?
        for entry in entries {
            if let value = entry.message(13)?.messages(3).first?.message(2)?.string(4) { uuid = value }
            if let raw = entry.message(9)?.message(1)?.int(2) { position = Int64(raw) }
            if let u = uuid, let p = position {
                order.append((p, u))
                uuid = nil
                position = nil
            }
        }
        let known = Set(children)
        let ordered = order.sorted { $0.0 < $1.0 }.map(\.1).filter { known.contains($0) }
        var seen = Set<String>()
        let result = (ordered + children).filter { seen.insert($0).inserted }
        return result
    }

    func attachmentFile(id: String) throws -> (URL, String) {
        try withDB { db, schema in
            guard let row = try attachmentRows(db, schema, identifiers: [id])[id], row["ent"]?.int == schema.ent("ICAttachment"),
                  let url = try fileFor(db, schema, attachment: row) else {
                throw APIError.notFound("Attachment not available.")
            }
            let type = UTType(filenameExtension: url.pathExtension.lowercased())?.preferredMIMEType ?? "application/octet-stream"
            return (url, type)
        }
    }

    // MARK: Diagnostics (no note content)

    func statistics() throws -> String {
        try withDB { db, schema in
            let folders = try loadFolders(db, schema)
            let notes = try loadNotes(db, schema, folders: folders)
            var blockCounts: [String: Int] = [:]
            var kinds: [String: Int] = [:]
            var servable: [String: Int] = [:]
            var emptyDecodes = 0
            for note in notes where !note.locked {
                if note.decoded.blocks.isEmpty { emptyDecodes += 1 }
                for block in note.decoded.blocks {
                    blockCounts[block.kind.rawValue, default: 0] += 1
                    if let a = block.attachment {
                        kinds[a.kind, default: 0] += 1
                        if a.url != nil { servable[a.kind, default: 0] += 1 }
                    }
                }
            }
            func fmt(_ d: [String: Int]) -> String { d.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: " ") }
            var lines: [String] = []
            lines.append("store: \(schema.store)")
            lines.append("folders: \(folders.count) (trash \(folders.filter(\.isTrash).count), smart \(folders.filter(\.isSmart).count), nested \(folders.filter { $0.parentPK != nil }.count))")
            lines.append("notes with hashtags/mentions/note links (not editable): \(notes.filter(\.decoded.hasInlineAttachments).count)")
            lines.append("notes: \(notes.count) (in trash \(notes.filter(\.inTrash).count), locked \(notes.filter(\.locked).count), pinned \(notes.filter(\.pinned).count), editable \(notes.filter(\.editable).count), with attachments \(notes.filter(\.decoded.hasAttachments).count), with checklists \(notes.filter(\.decoded.hasChecklist).count), no blocks decoded \(emptyDecodes))")
            lines.append("blocks: \(fmt(blockCounts))")
            lines.append("attachments by kind: \(fmt(kinds))")
            lines.append("attachments with servable bytes: \(fmt(servable))")
            let runs = notes.flatMap { $0.decoded.blocks.flatMap(\.runs) }
            lines.append("runs: total=\(runs.count) bold=\(runs.filter(\.bold).count) italic=\(runs.filter(\.italic).count) underline=\(runs.filter(\.underline).count) strike=\(runs.filter(\.strikethrough).count) link=\(runs.filter { $0.link != nil }.count)")
            let tables = notes.flatMap { $0.decoded.blocks.compactMap(\.rows) }
            lines.append("tables decoded: \(tables.count) (rows \(tables.map(\.count)), columns \(tables.map { $0.first?.count ?? 0 }))")
            return lines.joined(separator: "\n")
        }
    }
}

func percentEncode(_ text: String) -> String {
    var allowed = CharacterSet.alphanumerics
    allowed.insert(charactersIn: "-._~")
    return text.addingPercentEncoding(withAllowedCharacters: allowed) ?? text
}
