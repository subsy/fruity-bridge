import Darwin
import EventKit
import Foundation
import SQLite3

/// Runs before any EventKit, AppleScript or AppKit use. Fixtures are synthetic; storage is temporary.
enum SelfTests {
    private static var failures = 0
    private static var checks = 0

    private static func check(_ condition: @autoclosure () throws -> Bool, _ message: String) {
        checks += 1
        do {
            if try !condition() {
                failures += 1
                print("FAIL: \(message)")
            }
        } catch {
            failures += 1
            print("FAIL: \(message) (threw \(error))")
        }
    }

    static func run() -> Bool {
        gzipAndProtobuf()
        noteDecoding()
        importedHeadings()
        tableDecoding()
        markdown()
        dueDates()
        sorting()
        notesDeletion()
        auth()
        httpParsing()
        identifiers()
        configStorage()
        print(failures == 0 ? "Fruity Bridge self-tests passed (\(checks) checks)" : "Fruity Bridge self-tests: \(failures) of \(checks) checks failed")
        return failures == 0
    }

    // MARK: Fixture builders

    struct FixtureRun {
        var length: Int
        var style: Int? = nil
        var indent: Int? = nil
        var done: Int? = nil
        var quote = false
        var weight: Int? = nil
        var underline = false
        var strike = false
        var link: String? = nil
        var attachment: (String, String)? = nil
        var size: Float? = nil
    }

    static func noteMessage(_ text: String, _ runs: [FixtureRun]) -> [UInt8] {
        var note = ProtoWriter()
        note.string(2, text)
        for run in runs {
            note.message(5) { r in
                r.int(1, run.length)
                if run.style != nil || run.indent != nil || run.done != nil || run.quote {
                    r.message(2) { p in
                        if let style = run.style { p.int(1, style) }
                        if let indent = run.indent { p.int(4, indent) }
                        if let done = run.done { p.message(5) { c in c.bytes(1, Array(repeating: 7, count: 16)); c.int(2, done) } }
                        if run.quote { p.int(8, 1) }
                    }
                }
                if let size = run.size { r.message(3) { f in f.float(2, size) } }
                if let weight = run.weight { r.int(5, weight) }
                if run.underline { r.int(6, 1) }
                if run.strike { r.int(7, 1) }
                if let link = run.link { r.string(9, link) }
                if let (id, uti) = run.attachment { r.message(12) { a in a.string(1, id); a.string(2, uti) } }
            }
        }
        return note.out
    }

    static func noteStoreProto(_ text: String, _ runs: [FixtureRun]) -> [UInt8] {
        var root = ProtoWriter()
        root.message(2) { document in
            document.int(2, 0)
            document.bytes(3, noteMessage(text, runs))
        }
        return root.out
    }

    private static func u16(_ s: String) -> Int { s.utf16.count }

    // MARK: Tests

    static func gzipAndProtobuf() {
        let payload = Array("Hello, Notes! ".utf8) + Array(repeating: 65, count: 5000)
        let gz = Gzip.compress(payload)
        check(gz[0] == 0x1f && gz[1] == 0x8b, "gzip header written")
        check((try? Gzip.inflate(gz)) == payload, "gzip round trip")
        check(Gzip.crc32(Array("123456789".utf8)) == 0xCBF4_3926, "crc32 check value")
        check((try? Gzip.inflate([1, 2, 3, 4])) == nil, "non-gzip input rejected")
        var w = ProtoWriter()
        w.int(1, -1)
        w.int(2, 300)
        w.string(3, "héllo")
        w.message(4) { $0.int(1, 7) }
        let m = try? ProtoMessage(w.out)
        check(m?.int(1) == -1, "negative int32 decodes")
        check(m?.int(2) == 300, "multi-byte varint decodes")
        check(m?.string(3) == "héllo", "UTF-8 string decodes")
        check(m?.message(4)?.int(1) == 7, "nested message decodes")
        check((try? ProtoMessage([0x0A, 0x05, 0x01])) == nil, "truncated field rejected")
    }

    /// Notes stores AppleScript-imported <h1>/<h2> as bold body text at 24/18 pt.
    static func importedHeadings() {
        let lines = [
            ("Imported\n", [FixtureRun(length: 9, weight: 1, size: 24)]),
            ("Section\n", [FixtureRun(length: 8, weight: 1, size: 18)]),
            ("Bold line\n", [FixtureRun(length: 10, weight: 1)]),
            ("Big but not bold\n", [FixtureRun(length: 17, size: 24)]),
            ("Quoted\n", [FixtureRun(length: 7, quote: true, weight: 1, size: 24)]),
        ]
        guard let document = try? NoteDocument.decode(noteStoreProto: noteStoreProto(lines.map(\.0).joined(), lines.flatMap(\.1))) else {
            check(false, "imported-heading fixture decodes")
            return
        }
        let blocks = NoteBlockBuilder.build(document) { _, _ in .omit }.blocks
        let kinds = blocks.map(\.kind.rawValue)
        check(kinds == ["title", "heading", "body", "quote"], "24/18 pt bold body → title/heading: \(kinds)")
        check(blocks.first?.runs == [Run(text: "Imported")], "promoted title drops redundant bold")
        check(NoteMarkdown.render(Array(blocks.prefix(2))).hasPrefix("# Imported\n\n## Section"), "promoted headings render as # / ##")
    }

    static func noteDecoding() {
        // Title, bold run, link, checklist (done/undone), nested bullets, numbered list, monospace,
        // an image attachment, a hashtag and an emoji (UTF-16 surrogate pair).
        let lines = [
            ("Trip 🧳\n", [FixtureRun(length: u16("Trip 🧳\n"), style: 0)]),
            ("Bring passports\n", [FixtureRun(length: 6), FixtureRun(length: 9, weight: 1), FixtureRun(length: 1)]),
            ("Second line\n", [FixtureRun(length: u16("Second line\n"))]),
            ("\n", [FixtureRun(length: 1)]),
            ("Milk\n", [FixtureRun(length: 5, style: 103, done: 1)]),
            ("Eggs\n", [FixtureRun(length: 5, style: 103, done: 0)]),
            ("Top\n", [FixtureRun(length: 4, style: 100)]),
            ("Nested\n", [FixtureRun(length: 7, style: 100, indent: 1)]),
            ("One\n", [FixtureRun(length: 4, style: 102)]),
            ("Two\n", [FixtureRun(length: 4, style: 102)]),
            ("code a\n", [FixtureRun(length: 7, style: 4)]),
            ("code b\n", [FixtureRun(length: 7, style: 4)]),
            ("\u{FFFC}\n", [FixtureRun(length: 1, attachment: ("IMG-1", "public.jpeg")), FixtureRun(length: 1)]),
            // The hashtag is an inline attachment: a U+FFFC placeholder rendered from the database's alt text.
            ("See site \u{FFFC}", [FixtureRun(length: 4), FixtureRun(length: 4, link: "https://example.com"), FixtureRun(length: 1),
                                   FixtureRun(length: 1, attachment: ("TAG-1", "com.apple.notes.inlinetextattachment.hashtag"))]),
        ]
        let gz = Gzip.compress(noteStoreProto(lines.map(\.0).joined(), lines.flatMap(\.1)))
        guard let raw = try? Gzip.inflate(gz), let document = try? NoteDocument.decode(noteStoreProto: raw) else {
            check(false, "note fixture decodes")
            return
        }
        let decoded = NoteBlockBuilder.build(document) { id, _ in
            switch id {
            case "IMG-1": return .block(Block(kind: .attachment, attachment: AttachmentRef(id: "IMG-1", kind: "image", uti: "public.jpeg", filename: "a.jpg", url: "/v1/notes/attachments/IMG-1")))
            case "TAG-1": return .inline(text: "#trip", link: nil)
            default: return .omit
            }
        }
        let kinds = decoded.blocks.map(\.kind.rawValue)
        check(kinds == ["title", "body", "checklist", "checklist", "bullet", "bullet", "numbered", "numbered", "monospace", "attachment", "body"], "block types: \(kinds)")
        guard decoded.blocks.count == 11 else { return }
        let b = decoded.blocks
        check(b[0].plainText == "Trip 🧳", "title text with emoji (UTF-16 lengths)")
        check(b[1].runs == [Run(text: "Bring "), Run(text: "passports", bold: true), Run(text: "\nSecond line")], "body runs merge lines: \(b[1].runs)")
        check(b[2].checked && !b[3].checked, "checklist done state")
        check(b[4].indent == 0 && b[5].indent == 1, "bullet indent")
        check(b[6].number == 1 && b[7].number == 2, "numbered list numbering")
        check(b[8].plainText == "code a\ncode b", "monospace lines merge")
        check(b[9].attachment?.url == "/v1/notes/attachments/IMG-1", "attachment block")
        check(b[10].runs == [Run(text: "See "), Run(text: "site", link: "https://example.com"), Run(text: " #trip")], "link run and inline hashtag: \(b[10].runs)")
        check(decoded.hasChecklist && decoded.hasAttachments && decoded.hasInlineAttachments, "checklist/attachment/inline flags")
        check(decoded.plainText.contains("#trip") && decoded.plainText.contains("passports"), "plain text for search")
        let json = b[6].json
        check(json["type"] as? String == "numbered" && json["number"] as? Int == 1 && json["indent"] as? Int == 0, "numbered JSON")
        let runJSON = b[1].runs[1].json
        check(runJSON["bold"] as? Bool == true && runJSON["italic"] == nil, "run JSON omits false flags")

        // Plain note → markdown (editable path) and quote/heading styles.
        let plain = noteStoreProto("Plan\nIntro\nShopping\nbuy *stuff*\nwise words\n", [
            FixtureRun(length: 5, style: 0), FixtureRun(length: 6, style: 1), FixtureRun(length: 9, style: 2),
            FixtureRun(length: 4), FixtureRun(length: 7, weight: 2), FixtureRun(length: 1), FixtureRun(length: 11, quote: true),
        ])
        if let doc = try? NoteDocument.decode(noteStoreProto: plain) {
            let blocks = NoteBlockBuilder.build(doc) { _, _ in .omit }.blocks
            check(blocks.map(\.kind) == [.title, .heading, .subheading, .body, .quote], "heading/subheading/quote styles")
            check(NoteMarkdown.render(blocks) == "# Plan\n\n## Intro\n\n### Shopping\n\nbuy *\\*stuff\\**\n\n> wise words\n", "markdown render: \(NoteMarkdown.render(blocks).debugDescription)")
        } else {
            check(false, "plain fixture decodes")
        }
    }

    static func tableDecoding() {
        // A 2×2 table in MergableDataProto form: uuids 0,1 = rows, 2,3 = columns.
        let uuids: [[UInt8]] = (0..<4).map { Array(repeating: UInt8($0 + 1), count: 16) }
        let keys = ["crTableColumnDirection", "crRows", "crColumns", "cellColumns", "self"]
        let types = ["com.apple.notes.ICTable", "com.apple.CRDT.NSUUID", "com.apple.notes.CRTable"]
        var entries: [[UInt8]] = []
        func add(_ build: (inout ProtoWriter) -> Void) -> Int {
            var w = ProtoWriter()
            build(&w)
            entries.append(w.out)
            return entries.count - 1
        }
        func objectID(_ w: inout ProtoWriter, field: Int, index: Int) { w.message(field) { $0.int(6, index) } }
        func uuidRef(_ uuidIndex: Int) -> Int {
            add { e in e.message(13) { m in m.int(1, 1); m.message(3) { me in me.int(1, 4); me.message(2) { v in v.int(2, uuidIndex) } } } }
        }
        func cell(_ text: String) -> Int { add { e in e.bytes(10, noteMessage(text, [FixtureRun(length: text.utf16.count)])) } }
        let rowRefs = (0..<2).map { uuidRef($0) }
        let colRefs = (2..<4).map { uuidRef($0) }
        func orderedSet(_ uuidIndices: [Int], _ refs: [Int]) -> Int {
            add { e in
                e.message(16) { set in
                    set.message(1) { ordering in
                        ordering.message(1) { array in
                            array.bytes(1, noteMessage("", []))
                            for (i, u) in uuidIndices.enumerated() { array.message(2) { a in a.int(1, i); a.bytes(2, uuids[u]) } }
                        }
                        ordering.message(2) { contents in
                            for ref in refs { contents.message(1) { el in objectID(&el, field: 1, index: ref); objectID(&el, field: 2, index: ref) } }
                        }
                    }
                    set.message(2) { _ in }
                }
            }
        }
        let rowsSet = orderedSet([0, 1], rowRefs)
        let colsSet = orderedSet([2, 3], colRefs)
        let cells = [["Tin", "Time"], ["Loaf", "60 min"]].map { $0.map(cell) }
        var columnDicts: [Int] = []
        for c in 0..<2 {
            columnDicts.append(add { e in
                e.message(6) { d in
                    for r in 0..<2 { d.message(1) { el in objectID(&el, field: 1, index: rowRefs[r]); objectID(&el, field: 2, index: cells[r][c]) } }
                }
            })
        }
        let cellColumns = add { e in
            e.message(6) { d in
                for c in 0..<2 { d.message(1) { el in objectID(&el, field: 1, index: colRefs[c]); objectID(&el, field: 2, index: columnDicts[c]) } }
            }
        }
        _ = add { e in
            e.message(13) { m in
                m.int(1, 0)
                for (key, target) in [(1, rowsSet), (2, colsSet), (3, cellColumns)] {
                    m.message(3) { me in me.int(1, key); objectID(&me, field: 2, index: target) }
                }
            }
        }
        var root = ProtoWriter()
        root.message(2) { object in
            object.int(2, 1)
            object.message(3) { data in
                for e in entries { data.bytes(3, e) }
                for k in keys { data.string(4, k) }
                for t in types { data.string(5, t) }
                for u in uuids { data.bytes(6, u) }
            }
        }
        let rows = try? NoteTableDecoder.rows(mergeableData: Gzip.inflate(Gzip.compress(root.out))) { $0.text }
        check(rows == [["Tin", "Time"], ["Loaf", "60 min"]], "table rows decode: \(String(describing: rows))")
        let block = Block(kind: .table, rows: rows)
        check((block.json["rows"] as? [[String]])?.count == 2, "table block JSON")
    }

    static func markdown() {
        let source = """
            # Weekend <plan>

            Saturday: **football at 10**, then *lunch* & ~~nap~~.
            Line two with [map](https://maps.apple.com/?q=a&b="x")

            ## Kit

            - Pack boots
              - Spare laces
            - Charge camera
            * Dash item
            1. First
            2. Second

            > Quote line

            ```
            SSID: <home>
              indented
            ```
            """
        let blocks = NoteMarkdown.parse(source)
        check(blocks.map(\.kind) == [.title, .body, .heading, .bullet, .bullet, .bullet, .dash, .numbered, .numbered, .quote, .monospace], "markdown parse kinds: \(blocks.map(\.kind))")
        guard blocks.count == 11 else { return }
        check(blocks[4].indent == 1 && blocks[8].number == 2, "markdown list indent/number")
        check(blocks[1].runs.contains(Run(text: "football at 10", bold: true)) && blocks[1].runs.contains(Run(text: "lunch", italic: true))
              && blocks[1].runs.contains(Run(text: "nap", strikethrough: true)), "inline styles parse")
        check(blocks[1].runs.last == Run(text: "map", link: "https://maps.apple.com/?q=a&b=\"x\""), "link parse")
        check(blocks[10].plainText == "SSID: <home>\n  indented", "code block keeps text")
        let html = NoteMarkdown.html(blocks)
        check(html.hasPrefix("<div><h1>Weekend &lt;plan&gt;</h1></div>"), "title HTML escaped")
        check(html.contains("<b>football at 10</b>") && html.contains("<i>lunch</i>") && html.contains(" &amp; <strike>nap</strike>"), "inline HTML")
        check(html.contains("<a href=\"https://maps.apple.com/?q=a&amp;b=&quot;x&quot;\">map</a>"), "href escaped")
        check(html.contains("<div>Saturday: ") && html.contains("</div><div>Line two"), "line break becomes a new paragraph")
        check(html.contains("<ul><li>Pack boots</li><ul><li>Spare laces</li></ul><li>Charge camera</li></ul><ul class=\"Apple-dash-list\"><li>Dash item</li></ul><ol><li>First</li><li>Second</li></ol>"), "list HTML: \(html)")
        check(html.contains("<blockquote><div>Quote line</div></blockquote>"), "quote HTML")
        check(html.contains("<div><tt>SSID: &lt;home&gt;</tt></div><div><tt>  indented</tt></div>"), "monospace HTML")
        check(!html.contains("<plan>") && !html.contains("<home>"), "no raw HTML passes through")
        // Round trip: markdown → blocks → markdown is stable.
        let rendered = NoteMarkdown.render(blocks)
        check(NoteMarkdown.render(NoteMarkdown.parse(rendered)) == rendered, "markdown round trip stable")
        let tricky = [Block(kind: .title, runs: [Run(text: "T")]),
                      Block(kind: .body, runs: [Run(text: "# not a heading\n- not a list\n3. not numbered\n> no quote *star* [x](y) \\ back")])]
        let reparsed = NoteMarkdown.parse(NoteMarkdown.render(tricky))
        check(reparsed == tricky, "escaping round trip: \(NoteMarkdown.render(tricky).debugDescription)")
        let boldItalic = [Block(kind: .body, runs: [Run(text: "a "), Run(text: "both", bold: true, italic: true), Run(text: " b")])]
        check(NoteMarkdown.parse(NoteMarkdown.render(boldItalic)) == boldItalic, "bold+italic round trip")
        check(NoteMarkdown.parse("a * b * c") == [Block(kind: .body, runs: [Run(text: "a * b * c")])], "spaced asterisks stay literal")
    }

    static func dueDates() {
        let allDay = try? ReminderMapping.components(fromDue: ["date": "2026-11-14"])
        check(allDay?.hour == nil && allDay?.day == 14, "all-day due parses")
        check((ReminderMapping.dueJSON(allDay) as? [String: String]) == ["date": "2026-11-14"], "all-day due JSON")
        let timed = try? ReminderMapping.components(fromDue: ["date": "2026-11-14", "time": "09:05", "timeZone": "Europe/London"])
        check((ReminderMapping.dueJSON(timed) as? [String: String]) == ["date": "2026-11-14", "time": "09:05", "timeZone": "Europe/London"], "timed due JSON")
        check(ReminderMapping.dueDate(timed) == RFC3339.parse("2026-11-14T09:05:00Z"), "timed due instant (London in November is UTC)")
        check(ReminderMapping.dueJSON(nil) is NSNull, "no due → null")
        // "Today" across DST changes in London: 23-hour and 25-hour days.
        let london = TimeZone(identifier: "Europe/London")!
        check(ReminderMapping.endOfDay(containing: RFC3339.parse("2026-03-29T12:00:00Z")!, in: london) == RFC3339.parse("2026-03-29T23:00:00Z"), "end of 2026-03-29 in London (23 h day)")
        check(ReminderMapping.endOfDay(containing: RFC3339.parse("2026-10-25T12:00:00Z")!, in: london) == RFC3339.parse("2026-10-26T00:00:00Z"), "end of 2026-10-25 in London (25 h day)")
        let spring = RFC3339.parse("2026-03-29T12:00:00Z")!
        check(!ReminderMapping.isDueToday(try? ReminderMapping.components(fromDue: ["date": "2026-03-30"]), now: spring, zone: london), "tomorrow isn't due today on the 23 h day")
        check(ReminderMapping.isDueToday(try? ReminderMapping.components(fromDue: ["date": "2026-03-29", "time": "23:30", "timeZone": "Europe/London"]), now: spring, zone: london), "late tonight is due today on the 23 h day")
        let autumn = RFC3339.parse("2026-10-25T12:00:00Z")!
        check(ReminderMapping.isDueToday(try? ReminderMapping.components(fromDue: ["date": "2026-10-25", "time": "23:30", "timeZone": "Europe/London"]), now: autumn, zone: london), "23:30 is due today on the 25 h day")
        check(!ReminderMapping.isDueToday(try? ReminderMapping.components(fromDue: ["date": "2026-10-26"]), now: autumn, zone: london), "tomorrow isn't due today on the 25 h day")
        check(ReminderMapping.isDueToday(try? ReminderMapping.components(fromDue: ["date": "2026-10-01"]), now: autumn, zone: london), "overdue counts as today")
        check(!ReminderMapping.isDueToday(nil, now: autumn, zone: london), "undated isn't due today")
        // Completed window: 30 calendar days back, or completedSince.
        check((try? ReminderMapping.completedCutoff(nil, now: RFC3339.parse("2026-04-15T12:00:00Z")!, zone: london)) == RFC3339.parse("2026-03-16T13:00:00Z"), "30 days back across the DST change keeps wall time")
        check((try? ReminderMapping.completedCutoff("2026-10-25", now: autumn, zone: london)) == RFC3339.parse("2026-10-24T23:00:00Z"), "completedSince is the start of that local day")
        for bad in ["2026-13-01", "2026-02-30", "26-1-1", "yesterday"] {
            check((try? ReminderMapping.completedCutoff(bad, now: autumn, zone: london)) == nil, "bad completedSince rejected: \(bad)")
        }
        check((try? ReminderMapping.components(fromDue: NSNull())) == .some(nil), "null clears due")
        for bad: Any in [["date": "14/11/2026"], ["date": "2026-13-01"], ["date": "2026-11-14", "time": "25:00"], ["date": "2026-11-14", "time": "09:00", "timeZone": "Mars/Base"], "tomorrow"] {
            check((try? ReminderMapping.components(fromDue: bad)) == nil, "invalid due rejected: \(bad)")
        }
        check(ReminderMapping.recurrenceText(frequency: .weekly, interval: 1, weekdays: []) == "Every week", "recurrence weekly")
        check(ReminderMapping.recurrenceText(frequency: .daily, interval: 2, weekdays: []) == "Every 2 days", "recurrence interval")
        check(ReminderMapping.recurrenceText(frequency: .weekly, interval: 1, weekdays: [2, 3, 4, 5, 6]) == "Every weekday", "recurrence weekdays")
        check(ReminderMapping.recurrenceText(frequency: .weekly, interval: 1, weekdays: [5, 2]) == "Every week on Mon, Thu", "recurrence days")
        check(RFC3339.sameSecond(RFC3339.parse("2026-11-13T09:30:00Z"), RFC3339.parse("2026-11-13T09:30:00.900Z")), "conflict check ignores sub-second")
        check(!RFC3339.sameSecond(RFC3339.parse("2026-11-13T09:30:00Z"), RFC3339.parse("2026-11-13T09:30:01Z")), "conflict check detects change")
        check(RFC3339.string(Date(timeIntervalSince1970: 0)) as? String == "1970-01-01T00:00:00Z", "RFC 3339 output")
    }

    static func sorting() {
        let day: TimeInterval = 86400
        let base = Date(timeIntervalSince1970: 1_800_000_000)
        typealias K = ReminderMapping.SortKey
        let items: [(String, K)] = [
            ("done", K(completed: true, due: base, created: base)),
            ("undated-old", K(completed: false, due: nil, created: base - 5 * day)),
            ("due-later", K(completed: false, due: base + day, created: base)),
            ("undated-new", K(completed: false, due: nil, created: base)),
            ("due-soon", K(completed: false, due: base, created: base)),
            ("due-soon-older", K(completed: false, due: base, created: base - day)),
        ]
        let order = items.sorted { ReminderMapping.precedes($0.1, $1.1) }.map(\.0)
        check(order == ["due-soon-older", "due-soon", "due-later", "undated-old", "undated-new", "done"], "reminder sort: \(order)")

        func note(_ pk: Int64, pinned: Bool, modified: TimeInterval) -> NoteEntry {
            NoteEntry(pk: pk, id: "\(pk)", folderPK: 1, folderID: "f", title: "t", snippet: nil, createdAt: nil,
                      modifiedAt: Date(timeIntervalSince1970: modified), pinned: pinned, locked: false, inTrash: false,
                      decoded: DecodedNote(blocks: [], plainText: "", hasAttachments: false, hasChecklist: false))
        }
        let notes = NotesStore.sortSummaries([note(1, pinned: false, modified: 10), note(2, pinned: true, modified: 1), note(3, pinned: false, modified: 20)])
        check(notes.map(\.pk) == [2, 3, 1], "notes sort pinned then newest")
        let editable = note(4, pinned: false, modified: 0)
        check(editable.editable && editable.fullJSON["markdown"] != nil, "plain note is editable with markdown")
        var locked = editable
        locked.locked = true
        check(!locked.editable && locked.fullJSON["markdown"] == nil && (locked.fullJSON["blocks"] as? [Any])?.isEmpty == true, "locked note: no markdown, no blocks")
        var trashed = editable
        trashed.inTrash = true
        check(!trashed.editable, "trash isn't editable")
        var tagged = editable
        tagged.decoded.hasInlineAttachments = true
        check(!tagged.editable && tagged.summaryJSON["hasAttachments"] as? Bool == false, "tagged note isn't editable but has no attachments")
    }

    static func auth() {
        let secret = "0123abcd"
        var config = BridgeConfig()
        config.tailnetURL = "https://mini.example.ts.net:10000"
        config.pathSecret = secret
        config.allowedLogins = ["owner@example.com"]
        let tailnetHost = "mini.example.ts.net:10000"
        func request(_ path: String, login: String?, method: String = "GET", host: String? = tailnetHost, extra: [String: String] = [:]) -> HTTPRequest {
            var headers = extra
            if let login { headers["tailscale-user-login"] = login }
            if let host { headers["host"] = host }
            return HTTPRequest(method: method, rawPath: path, query: [:], headers: headers, body: Data())
        }
        func status(_ r: HTTPRequest, _ c: BridgeConfig = config) -> Int? {
            var r = r
            return RequestGuard.authorize(&r, config: c)?.status
        }
        let via = "/\(secret)"
        check(status(request(via + "/v1/health", login: nil)) == nil, "health needs no login")
        check(status(request("/v1/health", login: nil, host: "127.0.0.1:47380")) == nil, "local health without secret")
        check(status(request(via + "/v1/notes", login: nil)) == 401, "missing login → 401")
        check(status(request(via + "/v1/notes", login: "eve@example.com")) == 403, "other login → 403")
        check(status(request(via + "/v1/notes", login: "Owner@Example.com")) == nil, "allowed login (case-insensitive)")
        check(status(request(via + "/v1/events", login: "")) == 401, "empty login → 401")
        // Same-host forgery: a local process can set the login header but doesn't know the secret.
        check(status(request("/v1/notes", login: "owner@example.com", host: "127.0.0.1:47380")) == 401, "login header without path secret → 401")
        check(status(request("/0123abce/v1/notes", login: "owner@example.com")) == 401, "wrong path secret → 401")
        check(status(request(via + "x/v1/notes", login: "owner@example.com")) == 401, "secret must be a whole segment")
        var noSecret = config
        noSecret.pathSecret = nil
        check(status(request("/v1/notes", login: "owner@example.com"), noSecret) == 401, "no configured secret fails closed")
        var stripped = request(via + "/v1/notes/abc", login: "owner@example.com")
        check(RequestGuard.authorize(&stripped, config: config) == nil && stripped.rawPath == "/v1/notes/abc", "secret stripped from path")
        check(RequestGuard.stripSecret(via, secret: secret) == "/", "bare secret is the root")
        // DNS rebinding / browser protection, before identity.
        check(status(request(via + "/v1/notes", login: "owner@example.com", extra: ["origin": "https://evil.example"])) == 403, "Origin → 403")
        check(status(request(via + "/v1/health", login: nil, extra: ["sec-fetch-site": "cross-site"])) == 403, "Sec-Fetch-Site → 403 (even health)")
        check(status(request(via + "/v1/health", login: nil, extra: ["sec-fetch-mode": "cors"])) == 403, "Sec-Fetch-Mode → 403")
        check(status(request(via + "/v1/health", login: nil, host: "evil.example:10000")) == 403, "rebound Host → 403")
        check(status(request(via + "/v1/health", login: nil, host: "mini.example.ts.net")) == 403, "tailnet host without its port → 403")
        check(status(request(via + "/v1/health", login: nil, host: nil)) == 403, "missing Host → 403")
        check(status(request(via + "/v1/health", login: nil, host: "MINI.example.ts.net:10000")) == nil, "Host compare ignores case")
        check(status(request("/v1/health", login: nil, host: "localhost:47380")) == nil, "localhost:47380 allowed")
        check(status(request("/v1/health", login: nil, host: "127.0.0.1:8080")) == 403, "other local port → 403")
        var noTailnet = config
        noTailnet.tailnetURL = nil
        check(status(request(via + "/v1/health", login: nil), noTailnet) == 403, "tailnet Host needs tailnetURL")
        check(RequestGuard.authority(of: "https://my-mac.example-tailnet.ts.net:10000") == "my-mac.example-tailnet.ts.net:10000", "tailnet authority")

        // Router answers with the right codes without touching any store.
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fruity-bridge-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let router = Router(notes: NotesStore(container: directory), reminders: nil, config: Locked(config), notesAllowed: { false },
                            writeNotes: nil, notesWriteStatus: { "missing" })
        let health = router.handle(request(via + "/v1/health", login: nil))
        let healthJSON = (try? JSONSerialization.jsonObject(with: health.body)) as? [String: String]
        check(health.status == 200 && healthJSON?["version"] == bridgeVersion && healthJSON?["notes"] == "missing" && healthJSON?["reminders"] == "missing", "health JSON")
        check(router.handle(request(via + "/v1/reminders/lists", login: "owner@example.com")).status == 503, "reminders without permission → permission_missing")
        let notesResponse = router.handle(request(via + "/v1/notes/folders", login: "owner@example.com"))
        let error = ((try? JSONSerialization.jsonObject(with: notesResponse.body)) as? [String: Any])?["error"] as? [String: Any]
        check(error?["code"] as? String == "permission_missing" && error?["message"] as? String == PermissionMessage.notesDatabase, "notes without access → permission_missing")
        check(router.handle(request(via + "/v1/nope", login: "owner@example.com")).status == 404, "unknown endpoint → 404")
        check(router.handle(request(via + "/v1/events", login: "owner@example.com")).isEventStream, "events endpoint streams")
        let head = router.handle(request(via + "/v1/events", login: "owner@example.com", method: "HEAD"))
        check(!head.isEventStream && head.status == 200, "HEAD /v1/events doesn't open a stream")
        check(router.handle(request(via + "/v1/notes", login: "owner@example.com", method: "PATCH")).status == 405, "unsupported method → 405")
        // A missing database with access "allowed" is an error, not a crash.
        let missing = Router(notes: NotesStore(container: directory), reminders: nil, config: Locked(config), notesAllowed: { true },
                             writeNotes: nil, notesWriteStatus: { "missing" })
        check([500, 503].contains(missing.handle(request(via + "/v1/notes", login: "owner@example.com")).status), "missing database handled")

        // PUT without expectedModifiedAt is refused before Notes or the database is touched.
        let wrote = Locked(false)
        let writer = Router(notes: NotesStore(container: directory), reminders: nil, config: Locked(config), notesAllowed: { true },
                            writeNotes: { _ in wrote.set(true) }, notesWriteStatus: { "ok" })
        var put = request(via + "/v1/notes/x-coredata%3A%2F%2FA%2FICNote%2Fp1", login: "owner@example.com", method: "PUT")
        put.body = Data(#"{"markdown": "Hello"}"#.utf8)
        let putResponse = writer.handle(put)
        check(putResponse.status == 400 && !wrote.get(), "PUT without expectedModifiedAt → 400")
        put.body = Data(#"{"markdown": "Hello", "expectedModifiedAt": "yesterday"}"#.utf8)
        check(writer.handle(put).status == 400 && !wrote.get(), "PUT with a bad expectedModifiedAt → 400")
        let readOnly = Router(notes: NotesStore(container: directory), reminders: nil, config: Locked(config), notesAllowed: { true },
                              writeNotes: nil, notesWriteStatus: { "missing" })
        put.body = Data(#"{"markdown": "Hello"}"#.utf8)
        check(readOnly.handle(put).status == 400, "PUT validation comes before write availability")
    }

    /// Exercise the DELETE route against a synthetic live database. The write callback models
    /// Notes acknowledging a command, including the silent no-op that originally returned 204.
    static func notesDeletion() {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fruity-bridge-delete-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var database: OpaquePointer?
            guard sqlite3_open(directory.appendingPathComponent("NoteStore.sqlite").path, &database) == SQLITE_OK,
                  let database else { check(false, "delete fixture opens"); return }
            defer { sqlite3_close(database) }
            func sql(_ statement: String) throws {
                guard sqlite3_exec(database, statement, nil, nil, nil) == SQLITE_OK else {
                    throw BridgeError("delete fixture SQL failed")
                }
            }
            try sql("""
                CREATE TABLE Z_METADATA (Z_UUID TEXT);
                INSERT INTO Z_METADATA VALUES ('DELETE-TEST');
                CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
                INSERT INTO Z_PRIMARYKEY VALUES (1, 'ICFolder'), (2, 'ICNote');
                CREATE TABLE ZICNOTEDATA (Z_PK INTEGER, Z_OPT INTEGER);
                CREATE TABLE ZICCLOUDSYNCINGOBJECT (
                    Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER DEFAULT 1,
                    ZFOLDER INTEGER, ZMARKEDFORDELETION INTEGER DEFAULT 0, ZTITLE1 TEXT,
                    ZNOTEDATA INTEGER, ZMODIFICATIONDATE1 REAL, ZFOLDERTYPE INTEGER,
                    ZIDENTIFIER TEXT, ZOWNER INTEGER, ZTITLE2 TEXT, ZISPASSWORDPROTECTED INTEGER);
                INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, Z_ENT, ZFOLDERTYPE, ZOWNER) VALUES
                    (1, 1, 1, 10), (2, 1, 1, 20), (3, 1, 0, 10), (4, 1, 0, 10);
                INSERT INTO ZICCLOUDSYNCINGOBJECT
                    (Z_PK, Z_ENT, ZFOLDER, ZTITLE1, ZMODIFICATIONDATE1, ZISPASSWORDPROTECTED)
                    VALUES (5, 2, 3, 'Synthetic delete test', 100, 0);
                """)
            let store = NotesStore(container: directory)
            var change = ""
            var writes = 0
            let router = Router(notes: store, reminders: nil, config: Locked(BridgeConfig()), notesAllowed: { true },
                                writeNotes: { _ in writes += 1; if !change.isEmpty { try sql(change) } },
                                notesWriteStatus: { "ok" })
            let id = NotesID.make(store: "DELETE-TEST", entity: "ICNote", pk: 5)
            check(try store.deletionFolders(noteID: id).source == NotesID.make(store: "DELETE-TEST", entity: "ICFolder", pk: 3), "DELETE uses the note's stored source folder")
            try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZOWNER=20 WHERE Z_PK=3")
            check(try store.deletionFolders(noteID: id).trash == NotesID.make(store: "DELETE-TEST", entity: "ICFolder", pk: 2), "DELETE selects trash by account identity")
            try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZOWNER=10 WHERE Z_PK=3")
            let request = HTTPRequest(method: "DELETE", rawPath: "/v1/notes/\(id)", query: [:], headers: [:], body: Data())
            func reset() throws {
                try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZFOLDER=3, ZMARKEDFORDELETION=0, ZMODIFICATIONDATE1=100 WHERE Z_PK=5")
            }
            try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZMARKEDFORDELETION=1 WHERE Z_PK=1")
            check(router.dispatch(request).status == 422 && writes == 0, "DELETE refuses an account without active trash before writing")
            try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZMARKEDFORDELETION=0 WHERE Z_PK=1")
            let start = ProcessInfo.processInfo.systemUptime
            let unchanged = router.dispatch(request)
            let error = ((try? JSONSerialization.jsonObject(with: unchanged.body)) as? [String: Any])?["error"] as? [String: Any]
            check(unchanged.status == 500 && error?["code"] as? String == "internal", "DELETE silent no-op returns a JSON error")
            let elapsed = ProcessInfo.processInfo.systemUptime - start
            check(elapsed >= 2.9 && elapsed < 5, "DELETE waits for the store but stops at the verification deadline")
            change = "UPDATE ZICCLOUDSYNCINGOBJECT SET ZFOLDER=1 WHERE Z_PK=5"
            check(router.dispatch(request).status == 204, "DELETE confirmed in trash returns 204 without a modifiedAt change")
            check(try store.entry(id: id).inTrash, "DELETE success keeps the note recoverable in trash")
            let beforeRetry = writes
            check(router.dispatch(request).status == 422 && writes == beforeRetry, "DELETE retry never invokes permanent deletion")
            try reset()
            change = "UPDATE ZICCLOUDSYNCINGOBJECT SET ZMODIFICATIONDATE1=200, ZFOLDER=4 WHERE Z_PK=5"
            check(router.dispatch(request).status == 500, "DELETE concurrent edit or move is not proof of deletion")
            try reset()
            change = "UPDATE ZICCLOUDSYNCINGOBJECT SET ZMARKEDFORDELETION=1 WHERE Z_PK=5"
            check(router.dispatch(request).status == 204, "DELETE note no longer readable after write returns 204")
            let beforeMissing = writes
            check(router.dispatch(request).status == 404 && writes == beforeMissing, "DELETE initially missing note returns 404 without writing")
            try reset()
            try sql("UPDATE ZICCLOUDSYNCINGOBJECT SET ZISPASSWORDPROTECTED=1 WHERE Z_PK=5")
            change = "UPDATE ZICCLOUDSYNCINGOBJECT SET ZFOLDER=1 WHERE Z_PK=5"
            check(router.dispatch(request).status == 204, "DELETE locked note preserves existing behavior")
            try reset()
            change = "DROP TABLE ZICNOTEDATA"
            check(router.dispatch(request).status == 500, "DELETE read failure must not count as confirmed deletion")
        } catch { check(false, "delete fixture: \(error)") }
    }

    static func httpParsing() {
        let raw = "PUT /v1/notes/x-coredata%3A%2F%2FABC%2FICNote%2Fp12?q=a+b%26c&recent=5 HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\nTailscale-User-Login: owner@example.com\r\n\r\n{}GET /next HTTP/1.1\r\n\r\n"
        guard case .request(let request, let consumed) = HTTPParser.parse(Array(raw.utf8)) else {
            check(false, "request parses")
            return
        }
        check(request.method == "PUT" && request.body == Data("{}".utf8), "method and body")
        check(request.query["q"] == "a b&c" && request.query["recent"] == "5", "query decoding")
        check(request.header("Tailscale-User-Login") == "owner@example.com", "header lookup case-insensitive")
        check(request.rawPath.removingPercentEncoding == "/v1/notes/x-coredata://ABC/ICNote/p12", "encoded id path")
        let rest = Array(raw.utf8)[consumed...]
        if case .request(let next, _) = HTTPParser.parse(Array(rest)) { check(next.rawPath == "/next", "pipelined request") } else { check(false, "pipelined request") }
        if case .incomplete = HTTPParser.parse(Array("GET / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc".utf8)) {} else { check(false, "partial body waits") }
        if case .invalid = HTTPParser.parse(Array("GARBAGE\r\n\r\n".utf8)) {} else { check(false, "garbage rejected") }
        if case .invalid = HTTPParser.parse(Array("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".utf8)) {} else { check(false, "chunked rejected") }

        // Size limits: 16 KB of headers, 1 MB of body (refused from the head alone).
        let bigHeader = "GET / HTTP/1.1\r\nX-Pad: " + String(repeating: "a", count: 17 * 1024)
        if case .invalid(let status, _) = HTTPParser.parseHead(Array(bigHeader.utf8)) { check(status == 431, "oversized headers → 431") } else { check(false, "oversized headers rejected without a terminator") }
        if case .invalid(let status, _) = HTTPParser.parseHead(Array((bigHeader + "\r\n\r\n").utf8)) { check(status == 431, "oversized complete headers → 431") } else { check(false, "oversized complete headers rejected") }
        let okHeader = "GET / HTTP/1.1\r\nX-Pad: " + String(repeating: "a", count: 15 * 1024) + "\r\n\r\n"
        if case .request = HTTPParser.parseHead(Array(okHeader.utf8)) {} else { check(false, "15 KB of headers accepted") }
        if case .invalid(let status, _) = HTTPParser.parseHead(Array("POST / HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n".utf8)) { check(status == 413, "body over 1 MB → 413") } else { check(false, "body over 1 MB rejected") }
        if case .request(_, _, let length) = HTTPParser.parseHead(Array("POST / HTTP/1.1\r\nContent-Length: 1048576\r\n\r\n".utf8)) { check(length == 1_048_576, "1 MB body allowed") } else { check(false, "1 MB body allowed") }

        // The header deadline covers the whole head, not each read: a client trickling a byte
        // every 50 ms is cut off at the deadline.
        var pair: [Int32] = [0, 0]
        guard socketpair(AF_UNIX, SOCK_STREAM, 0, &pair) == 0 else { check(false, "socketpair"); return }
        defer { close(pair[0]); close(pair[1]) }
        let writerFD = pair[1]
        let stop = Locked(false)
        Thread.detachNewThread {
            let bytes = Array("GET / HTTP/1.1\r\nX-Slow: ".utf8) + Array(repeating: UInt8(ascii: "a"), count: 200)
            for byte in bytes where !stop.get() {
                var b = byte
                _ = send(writerFD, &b, 1, 0)
                usleep(50_000)
            }
        }
        var reader = RequestReader(fd: pair[0])
        let started = Date()
        let outcome = reader.readHead(timeout: 0.6)
        let elapsed = Date().timeIntervalSince(started)
        stop.set(true)
        if case .refuse(let response) = outcome { check(response.status == 408, "slow headers → 408") } else { check(false, "slow headers refused") }
        check(elapsed >= 0.55 && elapsed < 1.5, "header deadline is overall (\(String(format: "%.2f", elapsed)) s)")

        // A complete head followed by its body reads through the same reader.
        var pair2: [Int32] = [0, 0]
        guard socketpair(AF_UNIX, SOCK_STREAM, 0, &pair2) == 0 else { check(false, "socketpair"); return }
        defer { close(pair2[0]); close(pair2[1]) }
        let message = Array("POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello".utf8)
        _ = message.withUnsafeBytes { send(pair2[1], $0.baseAddress, $0.count, 0) }
        var reader2 = RequestReader(fd: pair2[0])
        if case .head(let head, let length) = reader2.readHead(timeout: 1), head.rawPath == "/x", length == 5,
           case .body(let body) = reader2.readBody(length: length, timeout: 1) {
            check(body == Data("hello".utf8), "reader returns the body")
        } else {
            check(false, "reader reads head and body")
        }
        shutdown(pair2[1], SHUT_WR)
        if case .closed = reader2.readHead(timeout: 1) {} else { check(false, "closed connection ends the reader") }
    }

    static func identifiers() {
        let store = "FF25BBBD-D870-49C1-8723-102098FCFBB3"
        let id = NotesID.make(store: store, entity: "ICNote", pk: 345)
        check(id == "x-coredata://FF25BBBD-D870-49C1-8723-102098FCFBB3/ICNote/p345", "AppleScript id format")
        check(NotesID.parse(id, entity: "ICNote", store: store) == 345, "id parses")
        check(NotesID.parse("x-coredata:/FF25BBBD-D870-49C1-8723-102098FCFBB3/ICNote/p345", entity: "ICNote", store: store) == 345, "collapsed slashes accepted")
        check(NotesID.parse(id, entity: "ICFolder", store: store) == nil, "entity mismatch rejected")
        check(NotesID.parse("x-coredata://OTHER/ICNote/p345", entity: "ICNote", store: store) == nil, "other store rejected")
        check(NotesID.parse("x-coredata://\(store)/ICNote/p0", entity: "ICNote", store: store) == nil, "invalid pk rejected")
        check(percentEncode("x-coredata://A/B") == "x-coredata%3A%2F%2FA%2FB", "percent encoding")
        check(NotesStore.kind(uti: "public.jpeg", hasMedia: true) == "image" && NotesStore.kind(uti: "com.adobe.pdf", hasMedia: true) == "pdf"
              && NotesStore.kind(uti: "com.apple.drawing.2", hasMedia: false) == "drawing" && NotesStore.kind(uti: "public.url", hasMedia: false) == "link"
              && NotesStore.kind(uti: "com.apple.m4a-audio", hasMedia: true) == "audio" && NotesStore.kind(uti: "com.apple.quicktime-movie", hasMedia: true) == "video"
              && NotesStore.kind(uti: "com.apple.notes.table", hasMedia: false) == "table" && NotesStore.kind(uti: "com.example.thing", hasMedia: true) == "file",
              "attachment kinds")
    }

    static func configStorage() {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fruity-bridge-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let created = BridgeConfig.load(from: directory, createIfMissing: true)
        check(created.allowedLogins.isEmpty, "no allowed logins by default")
        check(FileManager.default.fileExists(atPath: directory.appendingPathComponent("config.json").path), "default config written")
        try? Data(#"{"allowedLogins": ["a@b.c"], "tailnetURL": "https://x:10000"}"#.utf8).write(to: directory.appendingPathComponent("config.json"))
        let loaded = BridgeConfig.load(from: directory, createIfMissing: true)
        check(loaded.allowedLogins == ["a@b.c"] && loaded.tailnetURL == "https://x:10000", "config loads")
        try? Data(#"{"tailnetURL": "https://y"}"#.utf8).write(to: directory.appendingPathComponent("config.json"))
        check(BridgeConfig.load(from: directory, createIfMissing: false).allowedLogins.isEmpty, "missing allowedLogins refuses everyone")
        let secret = try? BridgeConfig.ensurePathSecret(in: directory)
        check(secret?.count == 48, "path secret created")
        check((try? BridgeConfig.ensurePathSecret(in: directory)) == secret, "path secret stable")
        let loadedSecret = BridgeConfig.load(from: directory, createIfMissing: false)
        check(loadedSecret.pathSecret == secret && loadedSecret.tailnetURL == "https://y", "path secret saved alongside other settings")
        let mode = (try? FileManager.default.attributesOfItem(atPath: directory.appendingPathComponent("config.json").path))?[.posixPermissions] as? Int
        check(mode == 0o600, "config.json is 0600")
    }
}
