import AppKit
import EventKit
import Foundation

// MARK: - Pure helpers (self-tested)

enum ReminderMapping {
    static let gregorian = Calendar(identifier: .gregorian)

    /// DateComponents → `{"date": "YYYY-MM-DD", "time"?: "HH:MM", "timeZone"?: "Area/City"}` or null.
    static func dueJSON(_ components: DateComponents?) -> Any {
        guard let c = components, let year = c.year, let month = c.month, let day = c.day else { return NSNull() }
        var out: [String: Any] = ["date": String(format: "%04d-%02d-%02d", year, month, day)]
        if let hour = c.hour {
            out["time"] = String(format: "%02d:%02d", hour, c.minute ?? 0)
            if let zone = c.timeZone { out["timeZone"] = zone.identifier }
        }
        return out
    }

    /// Parses the API's `due` object. A time without a time zone uses the Mac's zone.
    static func components(fromDue value: Any) throws -> DateComponents? {
        if value is NSNull { return nil }
        guard let due = value as? [String: Any], let date = due["date"] as? String else {
            throw APIError.badRequest("`due` must be null or an object with `date` (YYYY-MM-DD).")
        }
        let parts = date.split(separator: "-").map { Int($0) }
        guard parts.count == 3, let year = parts[0], let month = parts[1], let day = parts[2],
              (1...12).contains(month), (1...31).contains(day), year > 1900 else {
            throw APIError.badRequest("`due.date` must be YYYY-MM-DD.")
        }
        var c = DateComponents()
        c.calendar = gregorian
        c.year = year
        c.month = month
        c.day = day
        if let time = due["time"] as? String {
            let hm = time.split(separator: ":").map { Int($0) }
            guard hm.count == 2, let hour = hm[0], let minute = hm[1], (0...23).contains(hour), (0...59).contains(minute) else {
                throw APIError.badRequest("`due.time` must be HH:MM (24-hour).")
            }
            c.hour = hour
            c.minute = minute
            if let zone = due["timeZone"] as? String {
                guard let tz = TimeZone(identifier: zone) else { throw APIError.badRequest("Unknown time zone \(zone).") }
                c.timeZone = tz
            } else {
                c.timeZone = TimeZone.current
            }
        } else if due["time"] != nil, !(due["time"] is NSNull) {
            throw APIError.badRequest("`due.time` must be HH:MM (24-hour).")
        }
        return c
    }

    /// The instant used for sorting and "due today": all-day dates sort at the start of that day in
    /// `localZone` (the Mac's zone).
    static func dueDate(_ components: DateComponents?, localZone: TimeZone = .current) -> Date? {
        guard let c = components, let year = c.year, let month = c.month, let day = c.day else { return nil }
        var calendar = gregorian
        calendar.timeZone = c.hour != nil ? (c.timeZone ?? localZone) : localZone
        return calendar.date(from: DateComponents(year: year, month: month, day: day, hour: c.hour ?? 0, minute: c.minute ?? 0))
    }

    /// Start of the day after the one containing `now`, by the calendar (23 or 25 hours on DST days).
    static func endOfDay(containing now: Date, in zone: TimeZone = .current) -> Date {
        var calendar = gregorian
        calendar.timeZone = zone
        let start = calendar.startOfDay(for: now)
        return calendar.date(byAdding: .day, value: 1, to: start) ?? start.addingTimeInterval(86400)
    }

    /// `due=today`: due before the end of today (so overdue counts too).
    static func isDueToday(_ components: DateComponents?, now: Date, zone: TimeZone = .current) -> Bool {
        guard let due = dueDate(components, localZone: zone) else { return false }
        return due < endOfDay(containing: now, in: zone)
    }

    /// Earliest completion date to return: `completedSince` (YYYY-MM-DD, start of that local day) or
    /// 30 calendar days before `now`.
    static func completedCutoff(_ since: String?, now: Date, zone: TimeZone = .current) throws -> Date {
        var calendar = gregorian
        calendar.timeZone = zone
        guard let since else { return calendar.date(byAdding: .day, value: -30, to: now) ?? now }
        let parts = since.split(separator: "-", omittingEmptySubsequences: false).map { Int($0) }
        guard since.count == 10, parts.count == 3, let year = parts[0], let month = parts[1], let day = parts[2],
              let date = calendar.date(from: DateComponents(year: year, month: month, day: day)),
              calendar.component(.month, from: date) == month, calendar.component(.day, from: date) == day else {
            throw APIError.badRequest("`completedSince` must be YYYY-MM-DD.")
        }
        return date
    }

    struct SortKey {
        var completed: Bool
        var due: Date?
        var created: Date?
    }

    /// Incomplete first; then dated before undated, earliest due first; then by creation date.
    static func precedes(_ a: SortKey, _ b: SortKey) -> Bool {
        if a.completed != b.completed { return !a.completed }
        switch (a.due, b.due) {
        case let (x?, y?) where x != y: return x < y
        case (.some, nil): return true
        case (nil, .some): return false
        default: break
        }
        return (a.created ?? .distantPast) < (b.created ?? .distantPast)
    }

    /// Short English summary of a recurrence rule ("Every week", "Every 2 days", "Every weekday").
    static func recurrenceText(frequency: EKRecurrenceFrequency, interval: Int, weekdays: [Int]) -> String {
        let names = [1: "Sun", 2: "Mon", 3: "Tue", 4: "Wed", 5: "Thu", 6: "Fri", 7: "Sat"]
        let unit: String
        switch frequency {
        case .daily: unit = "day"
        case .weekly: unit = "week"
        case .monthly: unit = "month"
        case .yearly: unit = "year"
        @unknown default: unit = "period"
        }
        if frequency == .weekly, interval == 1, Set(weekdays) == [2, 3, 4, 5, 6] { return "Every weekday" }
        if frequency == .weekly, interval == 1, Set(weekdays) == [1, 7] { return "Every weekend" }
        var text = interval <= 1 ? "Every \(unit)" : "Every \(interval) \(unit)s"
        if frequency == .weekly, !weekdays.isEmpty, weekdays.count < 7 {
            text += " on " + weekdays.sorted { ($0 + 5) % 7 < ($1 + 5) % 7 }.compactMap { names[$0] }.joined(separator: ", ")
        }
        return text
    }

    static func hexColor(_ color: NSColor?) -> Any {
        guard let rgb = color?.usingColorSpace(.sRGB) else { return NSNull() }
        func byte(_ v: CGFloat) -> Int { Int((max(0, min(1, v)) * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(rgb.redComponent), byte(rgb.greenComponent), byte(rgb.blueComponent))
    }
}

// MARK: - EventKit service

/// Reminders through EventKit. The store is created only once access is granted, so nothing here
/// prompts unless the user picks the menu's "Allow Reminders access…".
final class RemindersService {
    private var store: EKEventStore?
    private let queue = DispatchQueue(label: "reminders")
    private var observer: NSObjectProtocol?
    var onChange: (() -> Void)?

    static var status: EKAuthorizationStatus { EKEventStore.authorizationStatus(for: .reminder) }
    static var authorized: Bool { status == .fullAccess }

    var healthStatus: String { RemindersService.authorized ? "ok" : "missing" }

    /// Must be called only from an explicit user action.
    func requestAccess(completion: @escaping (Bool, Error?) -> Void) {
        let requester = store ?? EKEventStore()
        requester.requestFullAccessToReminders { granted, error in
            self.queue.async {
                if granted { self.store = nil } // recreate with the new authorization
                completion(granted, error)
            }
        }
    }

    private func readyStore() throws -> EKEventStore {
        guard RemindersService.authorized else {
            throw APIError.permission(PermissionMessage.reminders)
        }
        if let store { return store }
        let created = EKEventStore()
        observer = NotificationCenter.default.addObserver(forName: .EKEventStoreChanged, object: created, queue: nil) { [weak self] _ in
            self?.onChange?()
        }
        store = created
        return created
    }

    /// Starts change notifications if access is already granted (no prompt otherwise).
    func startIfAuthorized() {
        queue.async { _ = try? self.readyStore() }
    }

    private func sync<T>(_ body: (EKEventStore) throws -> T) throws -> T {
        try queue.sync { try body(try readyStore()) }
    }

    private func fetch(_ store: EKEventStore, _ predicate: NSPredicate) throws -> [EKReminder] {
        let semaphore = DispatchSemaphore(value: 0)
        let result = Locked<[EKReminder]?>(nil)
        store.fetchReminders(matching: predicate) { reminders in
            result.set(reminders ?? [])
            semaphore.signal()
        }
        guard semaphore.wait(timeout: .now() + 30) == .success, let reminders = result.get() else {
            throw APIError.internalError("Reminders didn't answer in time.")
        }
        return reminders
    }

    // MARK: JSON

    private func json(_ r: EKReminder) -> [String: Any] {
        var recurrence: Any = NSNull()
        if let rule = r.recurrenceRules?.first {
            recurrence = ReminderMapping.recurrenceText(frequency: rule.frequency, interval: rule.interval,
                                                       weekdays: rule.daysOfTheWeek?.map { $0.dayOfTheWeek.rawValue } ?? [])
        }
        return [
            "id": r.calendarItemIdentifier, "listId": r.calendar?.calendarIdentifier ?? "", "title": r.title ?? "",
            "notes": r.notes ?? NSNull(), "url": r.url?.absoluteString ?? NSNull(), "priority": r.priority,
            "completed": r.isCompleted, "completedAt": RFC3339.string(r.completionDate),
            "due": ReminderMapping.dueJSON(r.dueDateComponents), "recurring": r.hasRecurrenceRules, "recurrenceText": recurrence,
            "createdAt": RFC3339.string(r.creationDate), "modifiedAt": RFC3339.string(r.lastModifiedDate),
        ]
    }

    private func sorted(_ reminders: [EKReminder]) -> [EKReminder] {
        reminders.sorted {
            ReminderMapping.precedes(
                .init(completed: $0.isCompleted, due: ReminderMapping.dueDate($0.dueDateComponents), created: $0.creationDate),
                .init(completed: $1.isCompleted, due: ReminderMapping.dueDate($1.dueDateComponents), created: $1.creationDate))
        }
    }

    // MARK: Endpoints

    /// `counts: false` skips fetching every open reminder and answers `openCount: null`.
    func lists(counts withCounts: Bool = true) throws -> [[String: Any]] {
        try sync { store in
            let calendars = store.calendars(for: .reminder)
            var counts: [String: Int] = [:]
            if withCounts {
                let open = try fetch(store, store.predicateForIncompleteReminders(withDueDateStarting: nil, ending: nil, calendars: calendars))
                for reminder in open { if let id = reminder.calendar?.calendarIdentifier { counts[id, default: 0] += 1 } }
            }
            var order: [String] = []
            var groups: [String: [EKCalendar]] = [:]
            for calendar in calendars {
                let source = calendar.source?.sourceIdentifier ?? ""
                if groups[source] == nil { order.append(source) }
                groups[source, default: []].append(calendar)
            }
            return order.flatMap { groups[$0]! }.map { calendar in
                [
                    "id": calendar.calendarIdentifier, "title": calendar.title, "color": ReminderMapping.hexColor(calendar.color),
                    "account": calendar.source?.title ?? NSNull(), "editable": calendar.allowsContentModifications,
                    "openCount": withCounts ? counts[calendar.calendarIdentifier] ?? 0 : NSNull(),
                ] as [String: Any]
            }
        }
    }

    func reminders(query: [String: String]) throws -> [[String: Any]] {
        try sync { store in
            var calendars: [EKCalendar]?
            if let listID = query["list"], !listID.isEmpty {
                guard let calendar = store.calendar(withIdentifier: listID), calendar.allowedEntityTypes.contains(.reminder) else {
                    throw APIError.notFound("List not found.")
                }
                calendars = [calendar]
            }
            var results: [EKReminder]
            if let due = query["due"] {
                let incomplete = try fetch(store, store.predicateForIncompleteReminders(withDueDateStarting: nil, ending: nil, calendars: calendars))
                switch due {
                case "today":
                    let now = Date()
                    results = incomplete.filter { ReminderMapping.isDueToday($0.dueDateComponents, now: now) }
                case "scheduled":
                    results = incomplete.filter { ReminderMapping.dueDate($0.dueDateComponents) != nil }
                default:
                    throw APIError.badRequest("`due` must be today or scheduled.")
                }
            } else {
                let mode = query["completed"] ?? "false"
                guard ["false", "true", "all"].contains(mode) else { throw APIError.badRequest("`completed` must be false, true or all.") }
                let cutoff = try ReminderMapping.completedCutoff(query["completedSince"], now: Date())
                results = []
                if mode != "true" {
                    results += try fetch(store, store.predicateForIncompleteReminders(withDueDateStarting: nil, ending: nil, calendars: calendars))
                }
                if mode != "false" {
                    results += try fetch(store, store.predicateForCompletedReminders(withCompletionDateStarting: cutoff, ending: nil, calendars: calendars))
                }
            }
            return sorted(results).map(json)
        }
    }

    func reminder(id: String) throws -> [String: Any] {
        try sync { store in json(try find(store, id)) }
    }

    private func find(_ store: EKEventStore, _ id: String) throws -> EKReminder {
        guard let reminder = store.calendarItem(withIdentifier: id) as? EKReminder else { throw APIError.notFound("Reminder not found.") }
        return reminder
    }

    private func writableCalendar(_ store: EKEventStore, _ id: Any?) throws -> EKCalendar {
        guard let id = id as? String, let calendar = store.calendar(withIdentifier: id), calendar.allowedEntityTypes.contains(.reminder) else {
            throw APIError.badRequest("`listId` must name an existing Reminders list.")
        }
        guard calendar.allowsContentModifications else { throw APIError.notEditable("That list is read-only.") }
        return calendar
    }

    /// Applies the API fields in `body` to `reminder`.
    private func apply(_ body: [String: Any], to reminder: EKReminder, store: EKEventStore) throws {
        if let value = body["title"] {
            guard let title = value as? String, !title.trimmingCharacters(in: .whitespaces).isEmpty else { throw APIError.badRequest("`title` must be a non-empty string.") }
            reminder.title = title
        }
        if let value = body["notes"] {
            guard value is NSNull || value is String else { throw APIError.badRequest("`notes` must be a string or null.") }
            reminder.notes = (value as? String).flatMap { $0.isEmpty ? nil : $0 }
        }
        if let value = body["url"] {
            if value is NSNull || (value as? String)?.isEmpty == true {
                reminder.url = nil
            } else {
                guard let text = value as? String, let url = URL(string: text) else { throw APIError.badRequest("`url` must be a URL string or null.") }
                reminder.url = url
            }
        }
        if let value = body["priority"] {
            guard let priority = value as? Int, (0...9).contains(priority) else { throw APIError.badRequest("`priority` must be 0–9.") }
            reminder.priority = priority
        }
        if let value = body["completed"] {
            guard let completed = value as? Bool else { throw APIError.badRequest("`completed` must be true or false.") }
            reminder.isCompleted = completed
        }
        if body.keys.contains("listId") {
            reminder.calendar = try writableCalendar(store, body["listId"])
        }
        if let value = body["due"] {
            let oldDue = ReminderMapping.dueDate(reminder.dueDateComponents)
            let hadTime = reminder.dueDateComponents?.hour != nil
            let components = try ReminderMapping.components(fromDue: value)
            reminder.dueDateComponents = components
            // Reminders.app pairs a timed due date with an alarm at that time; keep that pairing.
            for alarm in reminder.alarms ?? [] where hadTime && alarm.absoluteDate != nil && alarm.absoluteDate == oldDue {
                reminder.removeAlarm(alarm)
            }
            if components?.hour != nil, let date = ReminderMapping.dueDate(components),
               !(reminder.alarms ?? []).contains(where: { $0.absoluteDate == date }) {
                reminder.addAlarm(EKAlarm(absoluteDate: date))
            }
        }
    }

    func create(_ body: [String: Any]) throws -> [String: Any] {
        try sync { store in
            guard body["title"] is String else { throw APIError.badRequest("`title` is required.") }
            let reminder = EKReminder(eventStore: store)
            reminder.calendar = try writableCalendar(store, body["listId"])
            try apply(body.filter { $0.key != "listId" && $0.key != "completed" }, to: reminder, store: store)
            do { try store.save(reminder, commit: true) } catch { throw APIError.internalError("Couldn't save the reminder: \(error.localizedDescription)") }
            return json(reminder)
        }
    }

    func update(id: String, _ body: [String: Any]) throws -> [String: Any] {
        try sync { store in
            let reminder = try find(store, id)
            if let expected = body["expectedModifiedAt"] as? String {
                guard RFC3339.sameSecond(RFC3339.parse(expected), reminder.lastModifiedDate) else {
                    throw APIError.conflict("This reminder changed on another device.", current: json(reminder))
                }
            }
            guard reminder.calendar?.allowsContentModifications ?? false else { throw APIError.notEditable("That list is read-only.") }
            try apply(body.filter { $0.key != "expectedModifiedAt" }, to: reminder, store: store)
            do { try store.save(reminder, commit: true) } catch { throw APIError.internalError("Couldn't save the reminder: \(error.localizedDescription)") }
            return json(reminder)
        }
    }

    func delete(id: String) throws {
        try sync { store in
            let reminder = try find(store, id)
            do { try store.remove(reminder, commit: true) } catch { throw APIError.internalError("Couldn't delete the reminder: \(error.localizedDescription)") }
        }
    }
}
