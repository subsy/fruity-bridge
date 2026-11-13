import Foundation

// MARK: - Model

struct Run: Equatable {
    var text: String
    var bold = false
    var italic = false
    var underline = false
    var strikethrough = false
    var link: String?

    func sameStyle(_ other: Run) -> Bool {
        bold == other.bold && italic == other.italic && underline == other.underline
            && strikethrough == other.strikethrough && link == other.link
    }

    var json: [String: Any] {
        var out: [String: Any] = ["text": text]
        if bold { out["bold"] = true }
        if italic { out["italic"] = true }
        if underline { out["underline"] = true }
        if strikethrough { out["strikethrough"] = true }
        if let link { out["link"] = link }
        return out
    }
}

struct AttachmentRef: Equatable {
    var id: String
    var kind: String
    var uti: String?
    var filename: String?
    var url: String?
    /// Link cards carry their target and title; scans list every page.
    var href: String?
    var title: String?
    var pages: [String]?
    /// When the attachment last changed, so clients can drop cached bytes.
    var modifiedAt: Date?

    var json: [String: Any] {
        var out: [String: Any] = ["id": id, "kind": kind]
        out["uti"] = uti ?? NSNull()
        out["filename"] = filename ?? NSNull()
        out["url"] = url ?? NSNull()
        if let href { out["href"] = href }
        if let title { out["title"] = title }
        if let pages { out["pages"] = pages }
        if let modifiedAt { out["modifiedAt"] = RFC3339.string(modifiedAt) }
        return out
    }
}

struct Block: Equatable {
    enum Kind: String { case title, heading, subheading, body, monospace, bullet, dash, numbered, checklist, quote, attachment, table }
    var kind: Kind
    var runs: [Run] = []
    var indent = 0
    var number = 0
    var checked = false
    var attachment: AttachmentRef?
    var rows: [[String]]?

    var isListItem: Bool { [.bullet, .dash, .numbered, .checklist].contains(kind) }
    var plainText: String { runs.map(\.text).joined() }

    var json: [String: Any] {
        var out: [String: Any] = ["type": kind.rawValue]
        switch kind {
        case .attachment: out["attachment"] = attachment?.json ?? NSNull()
        case .table: out["rows"] = rows ?? []
        default: out["runs"] = runs.map(\.json)
        }
        if isListItem { out["indent"] = indent }
        if kind == .numbered { out["number"] = number }
        if kind == .checklist { out["checked"] = checked }
        return out
    }
}

/// Appends `run`, merging it into the previous run when the styles match.
func appendRun(_ runs: inout [Run], _ run: Run) {
    guard !run.text.isEmpty else { return }
    if let last = runs.last, last.sameStyle(run) {
        runs[runs.count - 1].text += run.text
    } else {
        runs.append(run)
    }
}

// MARK: - Protobuf → attribute runs

/// One attribute run of a Notes document (see apple_cloud_notes_parser's notestore.proto).
struct NoteAttributeRun {
    var length = 0
    var styleType: Int? // paragraph style: 0 title, 1 heading, 2 subheading, 4 monospace, 100 dot, 101 dash, 102 numbered, 103 checklist
    var indent = 0
    var checklistDone: Bool?
    var blockQuote = false
    var bold = false
    var italic = false
    var underline = false
    var strikethrough = false
    var link: String?
    var attachmentID: String?
    var attachmentUTI: String?
    /// Explicit font size, if any. Notes' HTML import (our AppleScript writes) turns <h1>/<h2> into
    /// bold body text at 24/18 pt rather than the Title/Heading styles, so sizes identify them.
    var pointSize: Float?
}

struct NoteDocument {
    var text: String
    var runs: [NoteAttributeRun]

    /// Decodes gunzipped ZICNOTEDATA.ZDATA (NoteStoreProto → Document → Note).
    static func decode(noteStoreProto bytes: [UInt8]) throws -> NoteDocument {
        let root = try ProtoMessage(bytes)
        guard let document = root.message(2), let note = document.message(3) else {
            throw BridgeError("missing document/note in NoteStoreProto")
        }
        return from(noteMessage: note)
    }

    static func from(noteMessage note: ProtoMessage) -> NoteDocument {
        let text = note.string(2) ?? ""
        let runs = note.messages(5).map { message -> NoteAttributeRun in
            var run = NoteAttributeRun()
            run.length = max(0, message.int(1) ?? 0)
            if let style = message.message(2) {
                if let type = style.int(1), type >= 0 { run.styleType = type }
                run.indent = max(0, style.int(4) ?? 0)
                if let checklist = style.message(5) { run.checklistDone = (checklist.int(2) ?? 0) != 0 }
                run.blockQuote = (style.int(8) ?? 0) != 0
            }
            if let font = message.message(3), let size = font.float(2), size > 0 { run.pointSize = size }
            let weight = message.int(5) ?? 0
            run.bold = weight == 1 || weight == 3
            run.italic = weight == 2 || weight == 3
            run.underline = (message.int(6) ?? 0) != 0
            run.strikethrough = (message.int(7) ?? 0) != 0
            if let link = message.string(9), !link.isEmpty { run.link = link }
            if let info = message.message(12) {
                run.attachmentID = info.string(1)
                run.attachmentUTI = info.string(2)
            }
            return run
        }
        return NoteDocument(text: text, runs: runs)
    }
}

// MARK: - Attribute runs → blocks

/// How an attachment character (U+FFFC) in the note text should be rendered.
enum ResolvedAttachment {
    case inline(text: String, link: String?)
    case block(Block)
    case omit
}

struct DecodedNote {
    var blocks: [Block]
    var plainText: String
    var hasAttachments: Bool
    var hasChecklist: Bool
    /// Hashtags, mentions and note links. Rewriting the note's HTML would turn them into plain
    /// text (and drop the note from tag smart folders), so such notes aren't editable.
    var hasInlineAttachments = false
    /// Notes' HTML import drops block quotes, so a save from Linux would flatten them.
    var hasQuote = false
}

enum NoteBlockBuilder {
    static let inlineUTIPrefix = "com.apple.notes.inlinetextattachment"

    private struct Segment {
        var units: [UInt16]
        var run: NoteAttributeRun
    }

    private struct Paragraph {
        var segments: [Segment] = []
        var style = NoteAttributeRun()
    }

    static func build(_ document: NoteDocument, resolve: (String, String?) -> ResolvedAttachment) -> DecodedNote {
        let units = Array(document.text.utf16)
        var paragraphs: [Paragraph] = []
        var current = Paragraph()
        var styleChosen = false
        var position = 0

        func consume(_ slice: ArraySlice<UInt16>, _ run: NoteAttributeRun) {
            var pending: [UInt16] = []
            for unit in slice {
                if unit == 0x0A {
                    if !pending.isEmpty { current.segments.append(Segment(units: pending, run: run)) }
                    pending = []
                    current.style = run // the paragraph's newline carries its paragraph style
                    paragraphs.append(current)
                    current = Paragraph()
                    styleChosen = false
                } else {
                    pending.append(unit)
                }
            }
            if !pending.isEmpty {
                if !styleChosen { current.style = run; styleChosen = true }
                current.segments.append(Segment(units: pending, run: run))
            }
        }

        for run in document.runs where position < units.count {
            let end = min(units.count, position + run.length)
            consume(units[position..<end], run)
            position = end
        }
        if position < units.count { consume(units[position...], NoteAttributeRun()) }
        if !current.segments.isEmpty { paragraphs.append(current) }

        var blocks: [Block] = []
        var plain: [String] = []
        var hasAttachments = false
        var hasChecklist = false
        var hasQuote = false
        var hasInline = false
        var numberCounters: [Int: Int] = [:]
        var pendingMonospaceBlankLines = 0
        var previousParagraphWasEmpty = true

        func kind(for style: NoteAttributeRun) -> Block.Kind {
            switch style.styleType {
            case 0: return .title
            case 1: return .heading
            case 2, 3: return .subheading
            case 4: return .monospace
            case 100: return .bullet
            case 101: return .dash
            case 102: return .numbered
            case 103: return .checklist
            default: return style.blockQuote ? .quote : .body
            }
        }

        func emitText(_ runs: [Run], _ style: NoteAttributeRun, continuation: Bool) {
            let k = kind(for: style)
            if k == .checklist { hasChecklist = true }
            if k == .quote { hasQuote = true }
            let text = runs.map(\.text).joined()
            guard !text.isEmpty else { return }
            plain.append(text)
            // Consecutive body/quote/monospace lines form one block with line breaks.
            if continuation, [.body, .quote, .monospace].contains(k), let last = blocks.last, last.kind == k {
                var merged = last.runs
                let breaks = k == .monospace ? pendingMonospaceBlankLines + 1 : 1
                appendRun(&merged, Run(text: String(repeating: "\n", count: breaks)))
                for run in runs { appendRun(&merged, run) }
                blocks[blocks.count - 1].runs = merged
                pendingMonospaceBlankLines = 0
                return
            }
            pendingMonospaceBlankLines = 0
            var block = Block(kind: k, runs: runs)
            if block.isListItem {
                block.indent = style.indent
                numberCounters = numberCounters.filter { $0.key < style.indent || (k == .numbered && $0.key == style.indent) }
                if k == .numbered {
                    let next = (numberCounters[style.indent] ?? 0) + 1
                    numberCounters[style.indent] = next
                    block.number = next
                }
                if k == .checklist { block.checked = style.checklistDone ?? false }
            } else {
                numberCounters = [:]
            }
            blocks.append(block)
        }

        for var paragraph in paragraphs {
            if let promoted = sizedHeadingStyle(paragraph) {
                // The style already implies bold; keeping it would put ** inside the # line.
                paragraph.style.styleType = promoted
                for i in paragraph.segments.indices { paragraph.segments[i].run.bold = false }
            }
            var runs: [Run] = []
            var continuation = !previousParagraphWasEmpty
            var paragraphHadContent = false
            for segment in paragraph.segments {
                if let id = segment.run.attachmentID {
                    let uti = segment.run.attachmentUTI
                    let isInline = uti?.hasPrefix(inlineUTIPrefix) ?? false
                    switch resolve(id, uti) {
                    case .inline(let text, let link):
                        hasInline = true
                        var run = Run(text: text)
                        run.link = link ?? segment.run.link
                        appendRun(&runs, run)
                    case .block(let block):
                        if !runs.isEmpty {
                            emitText(runs, paragraph.style, continuation: continuation)
                            runs = []
                        }
                        hasAttachments = true
                        blocks.append(block)
                        if let rows = block.rows { plain.append(rows.map { $0.joined(separator: " ") }.joined(separator: "\n")) }
                        numberCounters = [:]
                        continuation = false
                        paragraphHadContent = true
                    case .omit:
                        if isInline { hasInline = true } else { hasAttachments = true }
                    }
                    // Any extra characters beyond the U+FFFC placeholder are kept as text.
                    let rest = segment.units.filter { $0 != 0xFFFC }
                    if !rest.isEmpty { appendRun(&runs, styledRun(rest, segment.run)) }
                } else {
                    let cleaned = segment.units.filter { $0 != 0xFFFC }
                    if !cleaned.isEmpty { appendRun(&runs, styledRun(cleaned, segment.run)) }
                }
            }
            if !runs.isEmpty {
                emitText(runs, paragraph.style, continuation: continuation)
                paragraphHadContent = true
            } else if kind(for: paragraph.style) == .checklist {
                hasChecklist = true
            }
            if paragraphHadContent {
                previousParagraphWasEmpty = false
            } else if kind(for: paragraph.style) == .monospace, blocks.last?.kind == .monospace, !previousParagraphWasEmpty {
                pendingMonospaceBlankLines += 1 // blank line inside a code block
            } else {
                previousParagraphWasEmpty = true
                pendingMonospaceBlankLines = 0
                numberCounters = [:]
            }
        }
        return DecodedNote(blocks: blocks, plainText: plain.joined(separator: "\n"), hasAttachments: hasAttachments,
                           hasChecklist: hasChecklist, hasInlineAttachments: hasInline, hasQuote: hasQuote)
    }

    /// Title (0) or heading (1) for a body paragraph whose text is all bold at the sizes Notes gives
    /// imported <h1> (24 pt) and <h2> (18 pt); nil otherwise.
    private static func sizedHeadingStyle(_ paragraph: Paragraph) -> Int? {
        guard paragraph.style.styleType == nil, !paragraph.style.blockQuote,
              paragraph.segments.contains(where: { ($0.run.pointSize ?? 0) >= 17 }) else { return nil }
        let text = paragraph.segments.filter { seg in seg.units.contains { $0 != 0x20 && $0 != 0x09 && $0 != 0xFFFC } }
        guard !text.isEmpty, text.allSatisfy({ $0.run.bold && $0.run.attachmentID == nil }) else { return nil }
        let sizes = text.compactMap(\.run.pointSize)
        guard sizes.count == text.count, let smallest = sizes.min() else { return nil }
        if smallest >= 22 { return 0 }
        if smallest >= 17 { return 1 }
        return nil
    }

    private static func styledRun(_ units: [UInt16], _ attributes: NoteAttributeRun) -> Run {
        Run(text: String(decoding: units, as: UTF16.self), bold: attributes.bold, italic: attributes.italic,
            underline: attributes.underline, strikethrough: attributes.strikethrough, link: attributes.link)
    }
}

// MARK: - Tables (com.apple.notes.table mergeable data)

enum NoteTableDecoder {
    /// Rebuilds a table from gunzipped ZMERGEABLEDATA1 (MergableDataProto), following
    /// apple_cloud_notes_parser's AppleNotesEmbeddedTable. `cellText` renders a cell's Note message.
    static func rows(mergeableData bytes: [UInt8], cellText: (NoteDocument) -> String) throws -> [[String]] {
        let root = try ProtoMessage(bytes)
        guard let object = root.message(2), let data = object.message(3) else { throw BridgeError("missing mergeable data object") }
        let entries = data.messages(3)
        let keys = data.all(4).map { String(decoding: $0.data, as: UTF8.self) }
        let types = data.all(5).map { String(decoding: $0.data, as: UTF8.self) }
        let uuids = data.all(6).map { Array($0.data) }

        func entry(_ objectID: ProtoMessage?) -> ProtoMessage? {
            guard let index = objectID?.int(6), index >= 0, index < entries.count else { return nil }
            return entries[index]
        }
        /// The UUID index a map entry points at (custom_map.map_entry[0].value.unsigned_integer_value).
        func target(_ entry: ProtoMessage?) -> Int? {
            guard let map = entry?.message(13), let first = map.messages(3).first, let value = first.message(2),
                  let raw = value.uint(2) else { return nil }
            return Int(raw)
        }
        func ordering(_ object: ProtoMessage?) -> [Int: Int] {
            var indices: [Int: Int] = [:]
            guard let set = object?.message(16), let order = set.message(1) else { return indices }
            var count = 0
            for attachment in order.message(1)?.messages(2) ?? [] {
                if let uuid = attachment.bytes(2), let index = uuids.firstIndex(of: Array(uuid)) {
                    indices[index] = count
                }
                count += 1
            }
            for element in order.message(2)?.messages(1) ?? [] {
                if let key = target(entry(element.message(1))), let value = target(entry(element.message(2))), let mapped = indices[key] {
                    indices[value] = mapped
                }
            }
            return indices
        }

        var direction = "CRTableColumnDirectionLeftToRight"
        if let directionKey = keys.firstIndex(of: "crTableColumnDirection") {
            for candidate in entries {
                if let first = candidate.message(13)?.messages(3).first, first.int(1) == directionKey + 1,
                   let value = first.message(2)?.string(4) {
                    direction = value
                }
            }
        }
        guard let table = entries.first(where: { e in
            guard let type = e.message(13)?.int(1), type >= 0, type < types.count else { return false }
            return types[type] == "com.apple.notes.ICTable"
        }), let map = table.message(13) else { throw BridgeError("no ICTable root") }

        var rowIndices: [Int: Int] = [:]
        var columnIndices: [Int: Int] = [:]
        var cellColumns: ProtoMessage?
        for item in map.messages(3) {
            guard let key = item.int(1), key >= 0, key < keys.count else { continue }
            let value = entry(item.message(2))
            switch keys[key] {
            case "crRows": rowIndices = ordering(value)
            case "crColumns": columnIndices = ordering(value)
            case "cellColumns": cellColumns = value
            default: break
            }
        }
        let rowCount = (rowIndices.values.max() ?? -1) + 1
        let columnCount = (columnIndices.values.max() ?? -1) + 1
        guard rowCount > 0, columnCount > 0 else { return [] }
        var grid = Array(repeating: Array(repeating: "", count: columnCount), count: rowCount)
        for column in cellColumns?.message(6)?.messages(1) ?? [] {
            guard let columnUUID = target(entry(column.message(1))), let c = columnIndices[columnUUID] else { continue }
            for row in entry(column.message(2))?.message(6)?.messages(1) ?? [] {
                guard let rowUUID = target(entry(row.message(1))), let r = rowIndices[rowUUID],
                      let note = entry(row.message(2))?.message(10) else { continue }
                grid[r][c] = cellText(NoteDocument.from(noteMessage: note))
            }
        }
        if direction == "CRTableColumnDirectionRightToLeft" { grid = grid.map { Array($0.reversed()) } }
        return grid
    }
}

// MARK: - Edit format (Markdown subset)

enum NoteMarkdown {
    /// Blocks → Markdown. Only meaningful for editable notes (no attachments/checklists).
    static func render(_ blocks: [Block]) -> String {
        var out = ""
        var previous: Block?
        for block in blocks {
            if let previous {
                out += previous.isListItem && block.isListItem ? "\n" : "\n\n"
            }
            let pad = String(repeating: "  ", count: block.indent)
            switch block.kind {
            case .title: out += "# " + inline(block.runs, escapeLineStarts: false)
            case .heading: out += "## " + inline(block.runs, escapeLineStarts: false)
            case .subheading: out += "### " + inline(block.runs, escapeLineStarts: false)
            case .body: out += inline(block.runs, escapeLineStarts: true)
            case .quote:
                out += inline(block.runs, escapeLineStarts: false).split(separator: "\n", omittingEmptySubsequences: false)
                    .map { "> " + $0 }.joined(separator: "\n")
            case .monospace: out += "```\n" + block.plainText + "\n```"
            case .bullet: out += pad + "- " + inline(block.runs, escapeLineStarts: false)
            case .dash: out += pad + "* " + inline(block.runs, escapeLineStarts: false)
            case .numbered: out += pad + "\(max(1, block.number)). " + inline(block.runs, escapeLineStarts: false)
            case .checklist: out += pad + (block.checked ? "- [x] " : "- [ ] ") + inline(block.runs, escapeLineStarts: false)
            case .attachment, .table: continue
            }
            previous = block
        }
        return out.isEmpty ? "" : out + "\n"
    }

    static func escape(_ text: String) -> String {
        var out = ""
        for ch in text {
            if "\\*~[]`".contains(ch) { out.append("\\") }
            out.append(ch)
        }
        return out
    }

    private static func escapeLineStart(_ line: String) -> String {
        let trimmed = line.drop(while: { $0 == " " })
        let lead = String(line.prefix(line.count - trimmed.count))
        if trimmed.hasPrefix("#") || trimmed.hasPrefix(">") || trimmed.hasPrefix("- ") || trimmed.hasPrefix("+ ") {
            return lead + "\\" + trimmed
        }
        if let dot = trimmed.firstIndex(of: "."), dot != trimmed.startIndex,
           trimmed[trimmed.startIndex..<dot].allSatisfy(\.isASCIIDigit),
           trimmed[trimmed.index(after: dot)...].hasPrefix(" ") {
            return lead + trimmed[trimmed.startIndex..<dot] + "\\" + trimmed[dot...]
        }
        return line
    }

    static func inline(_ runs: [Run], escapeLineStarts: Bool) -> String {
        var out = ""
        for run in runs {
            // Keep surrounding whitespace outside the emphasis markers.
            let isSpace: (Character) -> Bool = { $0 == " " || $0 == "\t" }
            let leading = String(run.text.prefix(while: isSpace))
            let trailing = String(run.text.reversed().prefix(while: isSpace).reversed())
            guard leading.count < run.text.count else { out += run.text; continue }
            let core = String(run.text.dropFirst(leading.count).dropLast(trailing.count))
            var text = escape(core)
            if run.bold && run.italic { text = "***\(text)***" } else if run.bold { text = "**\(text)**" } else if run.italic { text = "*\(text)*" }
            if run.strikethrough { text = "~~\(text)~~" }
            if let link = run.link { text = "[\(text)](\(link.replacingOccurrences(of: ")", with: "%29").replacingOccurrences(of: " ", with: "%20")))" }
            out += leading + text + trailing
        }
        guard escapeLineStarts else { return out }
        return out.split(separator: "\n", omittingEmptySubsequences: false).map { escapeLineStart(String($0)) }.joined(separator: "\n")
    }

    // MARK: Markdown → blocks

    static func parse(_ markdown: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var quote: [String] = []
        var code: [String]?
        var counters: [Int: Int] = [:]

        func flush() {
            if !paragraph.isEmpty {
                blocks.append(Block(kind: .body, runs: parseInline(paragraph.joined(separator: "\n"))))
                paragraph = []
            }
            if !quote.isEmpty {
                blocks.append(Block(kind: .quote, runs: parseInline(quote.joined(separator: "\n"))))
                quote = []
            }
        }

        let lines = markdown.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        for line in lines {
            if var open = code {
                if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    blocks.append(Block(kind: .monospace, runs: [Run(text: open.joined(separator: "\n"))].filter { !$0.text.isEmpty }))
                    code = nil
                } else {
                    open.append(line)
                    code = open
                }
                continue
            }
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                flush()
                code = []
                continue
            }
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                flush()
                counters = [:]
                continue
            }
            let spaces = line.prefix(while: { $0 == " " }).count
            let body = String(line.dropFirst(spaces))
            if let (kind, text, number) = listItem(body) {
                flush()
                let indent = spaces / 2
                counters = counters.filter { $0.key < indent || (kind == .numbered && $0.key == indent) }
                var block = Block(kind: kind, runs: parseInline(text), indent: indent)
                if kind == .numbered {
                    let next = counters[indent].map { $0 + 1 } ?? number
                    counters[indent] = next
                    block.number = next
                }
                blocks.append(block)
                continue
            }
            counters = [:]
            if spaces == 0, let (kind, text) = heading(body) {
                flush()
                blocks.append(Block(kind: kind, runs: parseInline(text)))
                continue
            }
            if spaces == 0, body.hasPrefix(">") {
                if !paragraph.isEmpty { flush() }
                var text = String(body.dropFirst())
                if text.hasPrefix(" ") { text.removeFirst() }
                quote.append(text)
                continue
            }
            if !quote.isEmpty { flush() }
            paragraph.append(line)
        }
        if let open = code { blocks.append(Block(kind: .monospace, runs: [Run(text: open.joined(separator: "\n"))].filter { !$0.text.isEmpty })) }
        flush()
        return blocks
    }

    private static func heading(_ line: String) -> (Block.Kind, String)? {
        for (prefix, kind) in [("### ", Block.Kind.subheading), ("## ", .heading), ("# ", .title)] where line.hasPrefix(prefix) {
            return (kind, String(line.dropFirst(prefix.count)))
        }
        return nil
    }

    private static func listItem(_ line: String) -> (Block.Kind, String, Int)? {
        if line.hasPrefix("- ") { return (.bullet, String(line.dropFirst(2)), 0) }
        if line.hasPrefix("* ") { return (.dash, String(line.dropFirst(2)), 0) }
        let digits = line.prefix(while: \.isASCIIDigit)
        if !digits.isEmpty, digits.count <= 9, line.dropFirst(digits.count).hasPrefix(". ") {
            return (.numbered, String(line.dropFirst(digits.count + 2)), Int(digits) ?? 1)
        }
        return nil
    }

    /// Inline Markdown: **bold**, *italic*, ***both***, ~~strike~~, [text](url), backslash escapes.
    static func parseInline(_ text: String) -> [Run] {
        var runs: [Run] = []
        parseInline(Array(text), base: Run(text: ""), into: &runs)
        return runs
    }

    private static func parseInline(_ chars: [Character], base: Run, into runs: inout [Run]) {
        var i = 0
        var literal = ""
        func flushLiteral() {
            if !literal.isEmpty {
                var run = base
                run.text = literal
                appendRun(&runs, run)
                literal = ""
            }
        }
        func find(_ marker: [Character], from start: Int) -> Int? {
            guard marker.count <= chars.count else { return nil }
            var j = start
            while j + marker.count <= chars.count {
                if chars[j] == "\\" { j += 2; continue }
                if Array(chars[j..<(j + marker.count)]) == marker, j > start { return j }
                j += 1
            }
            return nil
        }
        while i < chars.count {
            let c = chars[i]
            if c == "\\", i + 1 < chars.count, chars[i + 1].isASCII, chars[i + 1].isPunctuation || chars[i + 1].isSymbol {
                literal.append(chars[i + 1])
                i += 2
                continue
            }
            if c == "[", let close = find(["]", "("], from: i + 1), let end = find([")"], from: close + 1) {
                flushLiteral()
                var linked = base
                linked.link = String(chars[(close + 2)..<end])
                parseInline(Array(chars[(i + 1)..<close]), base: linked, into: &runs)
                i = end + 1
                continue
            }
            var matched = false
            for (marker, apply) in [
                (Array("***"), { (r: inout Run) in r.bold = true; r.italic = true }),
                (Array("**"), { (r: inout Run) in r.bold = true }),
                (Array("~~"), { (r: inout Run) in r.strikethrough = true }),
                (Array("*"), { (r: inout Run) in r.italic = true }),
            ] as [([Character], (inout Run) -> Void)] {
                guard i + marker.count <= chars.count, Array(chars[i..<(i + marker.count)]) == marker else { continue }
                let start = i + marker.count
                guard start < chars.count, chars[start] != " ", let close = find(marker, from: start) else { continue }
                flushLiteral()
                var styled = base
                apply(&styled)
                parseInline(Array(chars[start..<close]), base: styled, into: &runs)
                i = close + marker.count
                matched = true
                break
            }
            if matched { continue }
            literal.append(c)
            i += 1
        }
        flushLiteral()
    }

    // MARK: Blocks → Notes HTML

    static func html(_ blocks: [Block]) -> String {
        var out = ""
        var lists: [Block.Kind] = []
        func closeLists(to depth: Int) {
            while lists.count > depth { out += lists.removeLast() == .numbered ? "</ol>" : "</ul>" }
        }
        var previous: Block?
        for block in blocks {
            if block.isListItem {
                let depth = block.indent + 1
                closeLists(to: depth)
                if lists.count == depth, lists.last != block.kind { closeLists(to: depth - 1) }
                while lists.count < depth {
                    switch block.kind {
                    case .numbered: out += "<ol>"
                    case .dash: out += "<ul class=\"Apple-dash-list\">"
                    default: out += "<ul>"
                    }
                    lists.append(block.kind)
                }
                out += "<li>" + inlineHTML(block.runs) + "</li>"
                previous = block
                continue
            }
            closeLists(to: 0)
            // Notes paragraphs are lines; separate consecutive text paragraphs with an empty line.
            if let previous, [.body, .quote].contains(previous.kind), [.body, .quote].contains(block.kind) {
                out += "<div><br></div>"
            }
            let lines = splitLines(block.runs)
            switch block.kind {
            case .title: out += "<div><h1>" + inlineHTML(block.runs) + "</h1></div>"
            case .heading: out += "<div><h2>" + inlineHTML(block.runs) + "</h2></div>"
            case .subheading: out += "<div><h3>" + inlineHTML(block.runs) + "</h3></div>"
            case .body: out += lines.map(lineDiv).joined()
            case .quote: out += "<blockquote>" + lines.map(lineDiv).joined() + "</blockquote>"
            case .monospace:
                let text = block.plainText.split(separator: "\n", omittingEmptySubsequences: false)
                out += text.map { $0.isEmpty ? "<div><tt><br></tt></div>" : "<div><tt>" + escapeHTML(String($0)) + "</tt></div>" }.joined()
            default: break
            }
            previous = block
        }
        closeLists(to: 0)
        return out
    }

    private static func lineDiv(_ runs: [Run]) -> String {
        let html = inlineHTML(runs)
        return html.isEmpty ? "<div><br></div>" : "<div>" + html + "</div>"
    }

    /// Splits runs at newlines into lines of runs.
    private static func splitLines(_ runs: [Run]) -> [[Run]] {
        var lines: [[Run]] = [[]]
        for run in runs {
            let parts = run.text.split(separator: "\n", omittingEmptySubsequences: false)
            for (index, part) in parts.enumerated() {
                if index > 0 { lines.append([]) }
                if !part.isEmpty {
                    var piece = run
                    piece.text = String(part)
                    lines[lines.count - 1].append(piece)
                }
            }
        }
        return lines
    }

    static func inlineHTML(_ runs: [Run]) -> String {
        runs.map { run in
            var text = escapeHTML(run.text).replacingOccurrences(of: "\n", with: "<br>")
            if run.bold { text = "<b>\(text)</b>" }
            if run.italic { text = "<i>\(text)</i>" }
            if run.underline { text = "<u>\(text)</u>" }
            if run.strikethrough { text = "<strike>\(text)</strike>" }
            if let link = run.link { text = "<a href=\"\(escapeHTML(link))\">\(text)</a>" }
            return text
        }.joined()
    }

    static func escapeHTML(_ text: String) -> String {
        var out = ""
        out.reserveCapacity(text.count)
        for ch in text {
            switch ch {
            case "&": out += "&amp;"
            case "<": out += "&lt;"
            case ">": out += "&gt;"
            case "\"": out += "&quot;"
            case "'": out += "&#39;"
            default: out.append(ch)
            }
        }
        return out
    }
}

extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}
