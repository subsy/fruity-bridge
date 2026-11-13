import AppKit
import CoreServices
import Foundation

/// Writes notes through AppleScript (Automation access to Notes). Only used by the menu-bar app,
/// only in response to requests, and always on the main thread as NSAppleScript requires.
final class NotesWriter {
    private static let notesBundleID = "com.apple.Notes"
    // Four-char codes from OpenScripting/AE headers, spelled out so no Carbon import is needed.
    private static let appleScriptSuite: AEEventClass = 0x6173_6372 // 'ascr'
    private static let subroutineEvent: AEEventID = 0x7073_6272 // 'psbr'
    private static let subroutineName: AEKeyword = 0x736E_616D // 'snam'
    private static let directObject: AEKeyword = 0x2D2D_2D2D // '----'
    private static let wildCard: DescType = 0x2A2A_2A2A // '****'
    private static let notPermitted: OSStatus = -1743
    private static let wouldRequireConsent: OSStatus = -1744
    private static let processNotFound: OSStatus = -600

    private static let source = """
        on make_note(folderId, html)
            tell application id "com.apple.Notes"
                with timeout of 45 seconds
                    set newNote to make new note at folder id folderId with properties {body:html}
                    return id of newNote
                end timeout
            end tell
        end make_note

        on set_body(noteId, html)
            tell application id "com.apple.Notes"
                with timeout of 45 seconds
                    set body of note id noteId to html
                end timeout
            end tell
            return noteId
        end set_body

        -- A generic `delete` was acknowledged, but SQLite kept the note in its folder for minutes;
        -- it later showed up in the trash folder already marked for deletion (ZMARKEDFORDELETION=1).
        -- Use an explicit move and verify a recoverable note in Recently Deleted.
        on delete_note(noteId, trashFolderId)
            tell application id "com.apple.Notes"
                with timeout of 45 seconds
                    move note id noteId to folder id trashFolderId
                end timeout
            end tell
            return noteId
        end delete_note
        """

    private var script: NSAppleScript?
    private let known = Locked("missing")

    /// Automation status without prompting (safe off the main thread). Notes must be running for
    /// macOS to answer; when it isn't, the last known answer is reused.
    func automationStatus() -> String {
        let target = NSAppleEventDescriptor(bundleIdentifier: NotesWriter.notesBundleID)
        let status = AEDeterminePermissionToAutomateTarget(target.aeDesc, NotesWriter.wildCard, NotesWriter.wildCard, false)
        switch status {
        case noErr: known.set("ok")
        case NotesWriter.notPermitted, NotesWriter.wouldRequireConsent: known.set("missing")
        case NotesWriter.processNotFound: break
        default: known.set("error")
        }
        return known.get()
    }

    /// Asks macOS for Automation access (shows the consent prompt). Menu action only; blocks.
    static func requestAutomation() -> OSStatus {
        let target = NSAppleEventDescriptor(bundleIdentifier: notesBundleID)
        return AEDeterminePermissionToAutomateTarget(target.aeDesc, wildCard, wildCard, true)
    }

    private func call(_ handler: String, _ arguments: [String]) throws -> String? {
        if script == nil {
            guard let compiled = NSAppleScript(source: NotesWriter.source) else { throw APIError.internalError("AppleScript unavailable.") }
            var error: NSDictionary?
            guard compiled.compileAndReturnError(&error) else { throw APIError.internalError("AppleScript didn't compile: \(error ?? [:])") }
            script = compiled
        }
        let event = NSAppleEventDescriptor(
            eventClass: NotesWriter.appleScriptSuite, eventID: NotesWriter.subroutineEvent,
            targetDescriptor: NSAppleEventDescriptor(processIdentifier: ProcessInfo.processInfo.processIdentifier),
            returnID: AEReturnID(-1), transactionID: AETransactionID(0))
        event.setParam(NSAppleEventDescriptor(string: handler), forKeyword: NotesWriter.subroutineName)
        let list = NSAppleEventDescriptor.list()
        for (index, argument) in arguments.enumerated() { list.insert(NSAppleEventDescriptor(string: argument), at: index + 1) }
        event.setParam(list, forKeyword: NotesWriter.directObject)
        var error: NSDictionary?
        let result = script!.executeAppleEvent(event, error: &error)
        if let error {
            let number = (error[NSAppleScript.errorNumber] as? Int) ?? 0
            let message = (error[NSAppleScript.errorMessage] as? String) ?? "AppleScript error \(number)"
            if number == -1743 || number == -1744 {
                known.set("missing")
                throw APIError.permission(PermissionMessage.notesAutomation)
            }
            if number == -1728 { throw APIError.notFound("Notes couldn't find that note or folder.") }
            if number == -1712 { throw APIError.internalError("Notes didn't answer within 45 s; the change may still appear.") }
            throw APIError.internalError("Notes refused the change: \(message) (\(number)).")
        }
        known.set("ok")
        return result.stringValue
    }

    func makeNote(folderID: String, html: String) throws -> String {
        guard let id = try call("make_note", [folderID, html]), !id.isEmpty else { throw APIError.internalError("Notes didn't return the new note's id.") }
        return id
    }

    func setBody(noteID: String, html: String) throws { _ = try call("set_body", [noteID, html]) }
    func delete(noteID: String, trashFolderID: String) throws {
        _ = try call("delete_note", [noteID, trashFolderID])
    }
}
