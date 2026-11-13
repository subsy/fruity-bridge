import AppKit
import EventKit
import Foundation

enum Permissions {
    /// Full Disk Access probe that never prompts: the system TCC database is readable only with FDA,
    /// and a denied open fails silently (FDA has no consent dialog).
    static func fullDiskAccess() -> Bool {
        let fd = open("/Library/Application Support/com.apple.TCC/TCC.db", O_RDONLY)
        guard fd >= 0 else { return false }
        close(fd)
        return true
    }

    static func openSettings(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") { NSWorkspace.shared.open(url) }
    }
}

/// Polls the Notes store every 3 s and publishes `notes` events when it changes.
final class NotesPoller {
    private let store: NotesStore
    private let hub: EventHub
    private let allowed: () -> Bool
    private var last: String?
    private var timer: DispatchSourceTimer?

    init(store: NotesStore, hub: EventHub, allowed: @escaping () -> Bool) {
        self.store = store
        self.hub = hub
        self.allowed = allowed
    }

    func start() {
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "notes-poller"))
        timer.schedule(deadline: .now() + 1, repeating: 3)
        timer.setEventHandler { [weak self] in self?.tick() }
        timer.resume()
        self.timer = timer
    }

    private func tick() {
        guard allowed(), let signature = try? store.changeSignature() else { return }
        if let last, last != signature { hub.publish("notes") }
        last = signature
    }
}

// MARK: - Menu-bar app

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var statusItem: NSStatusItem!
    private let config = Locked(BridgeConfig.load(createIfMissing: true))
    private let notes = NotesStore()
    private let reminders = RemindersService()
    private let writer = NotesWriter()
    private var server: HTTPServer?
    private var serverError: String?
    private var poller: NotesPoller?
    private var notesCheck = "unknown"

    func applicationDidFinishLaunching(_ notification: Notification) {
        // launchd sets XPC_SERVICE_NAME to the job label for the processes it starts.
        if ProcessInfo.processInfo.environment["XPC_SERVICE_NAME"] == agentLabel {
            replaceOlderInstances()
        } else if handOffToLaunchAgent() {
            print("Handed off to the LaunchAgent copy; exiting")
            exit(0)
        }
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.button?.image = NSImage(systemSymbolName: "arrow.left.arrow.right.circle", accessibilityDescription: "Fruity Bridge")
        let menu = NSMenu()
        menu.delegate = self
        menu.autoenablesItems = false
        statusItem.menu = menu

        let config = self.config
        let writer = self.writer
        let notesAllowed: () -> Bool = { Permissions.fullDiskAccess() || config.get().notesDatabaseApproved == true }
        let router = Router(
            notes: notes, reminders: reminders, config: config, notesAllowed: notesAllowed,
            writeNotes: { work in
                // NSAppleScript is main-thread only.
                try DispatchQueue.main.sync { try work(writer) }
            },
            notesWriteStatus: { writer.automationStatus() })
        router.logRejections = true
        let server = HTTPServer(port: bridgePort, router: router)
        // An instance that was just told to quit may still hold the port for a moment.
        let attempts = 10
        for attempt in 1...attempts {
            do {
                try server.start()
                self.server = server
                break
            } catch where attempt < attempts {
                Thread.sleep(forTimeInterval: 0.5)
            } catch {
                serverError = "\(error)"
            }
        }
        let hub = server.events
        reminders.onChange = { hub.publishDebounced("reminders", delay: 0.5) }
        reminders.startIfAuthorized()
        let poller = NotesPoller(store: notes, hub: hub, allowed: notesAllowed)
        poller.start()
        self.poller = poller
        rebuild(menu)
    }

    func menuNeedsUpdate(_ menu: NSMenu) { rebuild(menu) }

    /// Opened from Finder, or relaunched by macOS ("Quit & Reopen" after granting Full Disk Access):
    /// start the LaunchAgent job instead, so the running copy is the one launchd restarts on a crash.
    /// No-op success if the job is already running. False when the agent isn't loaded; then this
    /// copy serves.
    private func handOffToLaunchAgent() -> Bool {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        task.arguments = ["kickstart", "gui/\(getuid())/\(agentLabel)"]
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice
        do { try task.run() } catch { return false }
        task.waitUntilExit()
        return task.terminationStatus == 0
    }

    /// Runs in the LaunchAgent's copy: stops any other copy holding the port (one started outside
    /// launchd before the job was (re)started, e.g. by an update).
    private func replaceOlderInstances() {
        let me = ProcessInfo.processInfo.processIdentifier
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: Bundle.main.bundleIdentifier ?? agentLabel)
            .filter { $0.processIdentifier != me }
        guard !others.isEmpty else { return }
        // Signals rather than NSRunningApplication.terminate(): that sends a quit Apple Event, which
        // can raise an Automation prompt nobody sees on the headless Mac.
        others.forEach { kill($0.processIdentifier, SIGTERM) }
        let deadline = Date().addingTimeInterval(5)
        while others.contains(where: { !$0.isTerminated }), Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }
        others.filter { !$0.isTerminated }.forEach { kill($0.processIdentifier, SIGKILL) }
    }

    private func info(_ title: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    private func action(_ title: String, _ selector: Selector, indent: Bool = true) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: selector, keyEquivalent: "")
        item.target = self
        item.indentationLevel = indent ? 1 : 0
        return item
    }

    private func rebuild(_ menu: NSMenu) {
        menu.removeAllItems()
        if let serverError {
            menu.addItem(info("Not serving: \(serverError)"))
        } else {
            menu.addItem(info("Serving on 127.0.0.1:\(bridgePort) · \(server?.events.clientCount ?? 0) live client(s)"))
        }
        menu.addItem(info(config.get().tailnetURL ?? "Tailnet URL not set (run install.sh)"))
        if config.get().pathSecret?.isEmpty != false { menu.addItem(info("tailscale serve not set up (run install.sh)")) }
        menu.addItem(.separator())

        let reminderStatus = RemindersService.status
        menu.addItem(info("Reminders: " + (reminderStatus == .fullAccess ? "allowed" : reminderStatus == .notDetermined ? "not yet allowed" : "not allowed")))
        if reminderStatus == .notDetermined {
            menu.addItem(action("Allow Reminders access…", #selector(allowReminders)))
        } else if reminderStatus != .fullAccess {
            menu.addItem(action("Open Reminders privacy settings…", #selector(openRemindersSettings)))
        }

        let fda = Permissions.fullDiskAccess()
        let approved = config.get().notesDatabaseApproved == true
        var notesLine = "Notes database (Full Disk Access): " + (fda ? "allowed" : "not allowed")
        if !fda && approved { notesLine += " · direct read: \(notesCheck)" }
        menu.addItem(info(notesLine))
        if !fda {
            menu.addItem(action("Open Full Disk Access settings…", #selector(openFullDiskAccess)))
            menu.addItem(action("Read Notes database now…", #selector(readNotesNow)))
        }

        let automation = writer.automationStatus()
        let automationText = automation == "ok" ? "allowed" : NSWorkspace.shared.runningApplications.contains { $0.bundleIdentifier == "com.apple.Notes" } ? "not allowed" : "unknown until Notes is open"
        menu.addItem(info("Notes editing (Automation): " + automationText))
        if automation != "ok" {
            menu.addItem(action("Allow Notes editing…", #selector(allowNotesEditing)))
            menu.addItem(action("Open Automation settings…", #selector(openAutomationSettings)))
        }

        menu.addItem(.separator())
        let copy = action("Copy tailnet URL", #selector(copyURL), indent: false)
        copy.isEnabled = config.get().tailnetURL != nil
        menu.addItem(copy)
        menu.addItem(NSMenuItem(title: "Quit Fruity Bridge", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
    }

    @objc private func allowReminders() {
        reminders.requestAccess { granted, _ in
            if granted { self.reminders.startIfAuthorized() }
        }
    }

    @objc private func openRemindersSettings() { Permissions.openSettings("Privacy_Reminders") }
    @objc private func openFullDiskAccess() { Permissions.openSettings("Privacy_AllFiles") }
    @objc private func openAutomationSettings() { Permissions.openSettings("Privacy_Automation") }

    /// Without Full Disk Access, reading Notes' container may make macOS ask whether Fruity Bridge
    /// may access data from other apps. Only done when the user picks this item.
    @objc private func readNotesNow() {
        // Start from the file: install.sh may have changed it since launch (e.g. --allow-login).
        var updated = BridgeConfig.load(createIfMissing: false)
        updated.notesDatabaseApproved = true
        config.set(updated)
        try? updated.save()
        let store = notes
        DispatchQueue.global().async {
            let status = store.healthStatus()
            DispatchQueue.main.async { self.notesCheck = status == "ok" ? "working" : "failed (\(status))" }
        }
    }

    @objc private func allowNotesEditing() {
        let notesApp = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Notes")
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        configuration.hides = true
        let ask = {
            DispatchQueue.global().async {
                let status = NotesWriter.requestAutomation()
                if status != noErr { DispatchQueue.main.async { Permissions.openSettings("Privacy_Automation") } }
            }
        }
        if let notesApp {
            NSWorkspace.shared.openApplication(at: notesApp, configuration: configuration) { _, _ in
                // Give Notes a moment to register for Apple events before asking.
                DispatchQueue.global().asyncAfter(deadline: .now() + 2) { ask() }
            }
        } else {
            ask()
        }
    }

    @objc private func copyURL() {
        guard let url = config.get().tailnetURL else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url, forType: .string)
    }
}

// MARK: - Entry point

/// The value following `flag`, or nil if the flag is absent; exits if the value is missing.
private func value(after flag: String, in arguments: [String]) -> String? {
    guard let index = arguments.firstIndex(of: flag) else { return nil }
    guard index + 1 < arguments.count else {
        FileHandle.standardError.write(Data("\(flag) needs a value\n".utf8))
        exit(2)
    }
    return arguments[index + 1]
}

/// Applies `change` to the saved config, prints its message and exits.
private func updateConfig(_ change: (inout BridgeConfig) -> String) -> Never {
    var config = BridgeConfig.load(createIfMissing: false)
    let message = change(&config)
    do {
        try config.save()
        print(message)
        exit(0)
    } catch {
        FileHandle.standardError.write(Data("Couldn't save config: \(error)\n".utf8))
        exit(1)
    }
}

@main
struct FruityBridgeMain {
    static func main() {
        setvbuf(stdout, nil, _IOLBF, 0)
        let arguments = CommandLine.arguments
        if arguments.contains("--selftest") {
            exit(SelfTests.run() ? 0 : 1)
        }
        if arguments.contains("--dump-notes") {
            do {
                let started = Date()
                let store = NotesStore()
                print(try store.statistics())
                print("change signature: \(try store.changeSignature().isEmpty ? "empty" : "ok")")
                print(String(format: "decoded in %.2f s", Date().timeIntervalSince(started)))
                exit(0)
            } catch let error as APIError {
                print("dump failed: \(error.code): \(error.message)")
            } catch {
                print("dump failed: \(error)")
            }
            exit(1)
        }
        if arguments.contains("--serve-notes-only") {
            serveNotesOnly()
        }
        if arguments.contains("--path-secret") {
            // install.sh puts this in the `tailscale serve` target; creates it on first use.
            do {
                print(try BridgeConfig.ensurePathSecret())
                exit(0)
            } catch {
                FileHandle.standardError.write(Data("Couldn't save config: \(error)\n".utf8))
                exit(1)
            }
        }
        if let url = value(after: "--set-tailnet-url", in: arguments) {
            updateConfig { $0.tailnetURL = url; return "Tailnet URL set to \(url)" }
        }
        if let login = value(after: "--allow-login", in: arguments) {
            updateConfig { config in
                if !config.allows(login) { config.allowedLogins.append(login) }
                return "Allowed logins: \(config.allowedLogins.joined(separator: ", ")) (a running bridge picks this up when restarted)"
            }
        }
        if arguments.contains("--help") || arguments.contains("-h") {
            print("Usage: FruityBridge [--selftest | --dump-notes | --serve-notes-only | --set-tailnet-url URL | --allow-login LOGIN | --path-secret]\nWithout options, runs the menu-bar app.")
            exit(0)
        }
        MainActor.assumeIsolated {
            let app = NSApplication.shared
            app.setActivationPolicy(.accessory)
            let delegate = AppDelegate()
            app.delegate = delegate
            withExtendedLifetime(delegate) { app.run() }
        }
    }

    /// Notes read endpoints + health only: no EventKit, no AppleScript, no GUI. Safe over SSH.
    static func serveNotesOnly() -> Never {
        let config = Locked(BridgeConfig.load(createIfMissing: false))
        let store = NotesStore()
        let router = Router(notes: store, reminders: nil, config: config, notesAllowed: { true }, writeNotes: nil, notesWriteStatus: { "missing" })
        router.logRejections = true
        let server = HTTPServer(port: bridgePort, router: router)
        do {
            try server.start()
        } catch {
            print("Couldn't start: \(error)")
            exit(1)
        }
        let poller = NotesPoller(store: store, hub: server.events, allowed: { true })
        poller.start()
        let serveState = config.get().pathSecret?.isEmpty == false ? "path secret set" : "NO path secret: only /v1/health answers (run --path-secret)"
        print("Fruity Bridge \(bridgeVersion): serving Notes (read-only) on 127.0.0.1:\(bridgePort); allowed logins: \(config.get().allowedLogins.joined(separator: ", ")); \(serveState)")
        withExtendedLifetime((server, poller, router)) { dispatchMain() }
    }
}
