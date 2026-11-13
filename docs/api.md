# Fruity Bridge HTTP API (v1)

The Mac bridge app listens on `127.0.0.1:47380` only. `tailscale serve` publishes it to the
tailnet at `https://<mac-name>.<tailnet>.ts.net:10000` (HTTPS, tailnet only).

## Auth

Every request must carry the `Tailscale-User-Login` header that `tailscale serve` adds. The bridge
rejects requests whose login isn't in its allow list (`allowedLogins` in its config; `install.sh`
adds the login the Mac's Tailscale is signed in with, and an empty list refuses everyone) with `403`. Requests without the header get `401`, except `GET /v1/health`.
Clients never send the header themselves; Tailscale sets it.

`tailscale serve` proxies to `http://127.0.0.1:47380/<pathSecret>`, so every forwarded request path
starts with a random secret from the bridge's `config.json` (0600). The bridge strips it and only
trusts `Tailscale-User-Login` on requests that carried it; local processes that don't know the
secret get `401`. Clients never see or send the secret.

Browser protection (DNS rebinding): the bridge rejects with `403` any request carrying an `Origin`
or `Sec-Fetch-Site`/`Sec-Fetch-Mode` header, and any request whose `Host` is neither the tailnet
host (from `tailnetURL` in its config) nor `127.0.0.1:47380`/`localhost:47380`.

Limits: request headers ≤ 16 KB (`431`), received within 10 s (`408`); bodies ≤ 1 MB (`413`) within 30 s; auth is checked before the
body is read; at most 32 concurrent connections and 8 event streams (oldest stream is closed).

## Conventions

- JSON, UTF-8. Field names camelCase. Absent optional fields are omitted or `null`; clients accept both.
- Timestamps are RFC 3339 strings with offset (`2026-11-13T09:30:00Z`).
- Errors: non-2xx with `{"error": {"code": "not_found", "message": "Human readable"}}`.
  Codes: `bad_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`, `not_editable`,
  `permission_missing` (macOS permission not granted; message says which; HTTP 503), `internal`.
  Unsupported methods get `405` with `bad_request`.
- IDs are opaque strings. Clients must URL-encode them in paths (they can contain `/` and `:`).

## Health

`GET /v1/health` → `200`
```json
{"version": "0.1.0", "reminders": "ok", "notes": "ok", "notesWrite": "ok"}
```
Each permission field is `ok`, `missing` or `error`. `notesWrite` reflects Automation access to Notes.

## Events

`GET /v1/events` → `text/event-stream`. Server-sent events, one JSON object per `data:` line.
```
event: reminders
data: {"changedAt": "2026-11-13T09:30:00Z"}

event: notes
data: {"changedAt": "2026-11-13T09:30:00Z"}
```
`reminders` fires on `EKEventStoreChanged` (debounced 500 ms). `notes` fires when the Notes store's
latest modification time changes (polled every 3 s). A `: ping` comment is sent every 20 s.
Clients refetch what they show when an event arrives, and reconnect with backoff on disconnect.

## Reminders

### Lists
`GET /v1/reminders/lists`
```json
{"lists": [
  {"id": "EK-calendar-identifier", "title": "Shopping", "color": "#FF9500",
   "account": "iCloud", "editable": true, "openCount": 12}
]}
```
Ordered as Apple's Reminders sidebar (EventKit order, grouped by account). `openCount` costs a
fetch of every open reminder; `?counts=false` omits it (`openCount: null`).

### Reminders
`GET /v1/reminders?list=<listId>&completed=<false|true|all>` (default `completed=false`).
`list` may be omitted to get all lists. Completed reminders are limited to those completed in the
last 30 days unless `completedSince=YYYY-MM-DD` is given. Also `GET /v1/reminders?due=today` (incomplete, due today or
overdue) and `?due=scheduled` (incomplete with any due date), across all lists.

```json
{"reminders": [
  {"id": "EK-calendarItemIdentifier", "listId": "…", "title": "Milk", "notes": "semi-skimmed",
   "url": null, "priority": 0, "completed": false, "completedAt": null,
   "due": {"date": "2026-11-14", "time": "09:00", "timeZone": "Europe/London"},
   "recurring": false, "recurrenceText": null,
   "createdAt": "…", "modifiedAt": "…"}
]}
```
- `priority`: 0 none, 1 high, 5 medium, 9 low (EventKit values).
- `due`: `null`, or `date` (YYYY-MM-DD) with optional `time` (HH:MM, 24h) and `timeZone` (IANA).
  No `time` means an all-day due date. A timed due date is wall-clock time in `timeZone`; clients
  convert to their local zone for display and send their own IANA zone when setting a time
  (a missing zone means the Mac's zone). A timed due date also gets an alarm at that time.
- `listId` is always a string.
- Clearing `notes` or `url`: send `null` or `""`.
- `recurrenceText`: short English summary such as "Every week" when `recurring`.
- Sort: incomplete first; then by due (dated first, earliest first), then by `createdAt`.

`POST /v1/reminders` body `{"listId", "title", "notes"?, "due"?, "priority"?, "url"?}` → `201` with
the reminder object.

`PATCH /v1/reminders/<id>` body: any of `title`, `notes`, `due` (`null` clears), `priority`,
`completed`, `listId`, `url`. Optional `expectedModifiedAt`: if present and the reminder's
`modifiedAt` differs, respond `409 conflict` with the current reminder in `error.current`.
→ `200` with the updated reminder.

`GET /v1/reminders/<id>` → the reminder.

`DELETE /v1/reminders/<id>` → `204`.

## Notes

Notes are read from the Notes database (needs Full Disk Access) and written through AppleScript
(needs Automation access to Notes).

### Folders
`GET /v1/notes/folders`
```json
{"folders": [
  {"id": "x-coredata://…/ICFolder/p12", "name": "Notes", "account": "iCloud",
   "parentId": null, "noteCount": 42, "isTrash": false, "isSmart": false}
]}
```
Includes nested folders (`parentId`). Trash is included with `isTrash: true`. Smart folders are
listed with `isSmart: true` and `noteCount` may be `null`.

### Note summaries
`GET /v1/notes?folder=<folderId>` or `GET /v1/notes?q=<search>` (title and body text, case-insensitive,
all folders except trash) or `GET /v1/notes?recent=50`.
```json
{"notes": [
  {"id": "x-coredata://…/ICNote/p345", "folderId": "…", "title": "Packing list",
   "snippet": "first line of body…", "createdAt": "…", "modifiedAt": "…",
   "pinned": false, "locked": false, "editable": true, "hasAttachments": false, "hasChecklist": false}
]}
```
Sorted pinned first, then `modifiedAt` descending. `editable` is true only when the note is not
locked, has no attachments (tables count as attachments), no checklist items, no hashtags, mentions
or note links, and is not in Trash. `GET /v1/notes` with no parameters returns all notes outside
Recently Deleted. Smart folders support tag-based queries; others answer `422 bad_request`.

### Note
`GET /v1/notes/<id>` → summary fields plus `blocks` and `markdown`:
```json
{"id": "…", "title": "…", "modifiedAt": "…", "editable": true, "locked": false,
 "blocks": [
   {"type": "title", "runs": [{"text": "Packing list"}]},
   {"type": "heading", "runs": [...]},
   {"type": "subheading", "runs": [...]},
   {"type": "body", "runs": [{"text": "Bring ", "bold": false}, {"text": "passports", "bold": true}]},
   {"type": "monospace", "runs": [...]},
   {"type": "bullet", "indent": 0, "runs": [...]},
   {"type": "dash", "indent": 0, "runs": [...]},
   {"type": "numbered", "indent": 0, "number": 1, "runs": [...]},
   {"type": "checklist", "indent": 0, "checked": true, "runs": [...]},
   {"type": "quote", "runs": [...]},
   {"type": "attachment", "attachment": {"id": "…", "kind": "image", "uti": "public.jpeg",
     "filename": "IMG_1234.jpeg", "url": "/v1/notes/attachments/<attachmentId>"}},
   {"type": "table", "rows": [["a", "b"], ["c", "d"]]}
 ],
 "markdown": "# Packing list\n\nBring **passports**\n"}
```
Run fields: `text` (required), `bold`, `italic`, `underline`, `strikethrough`, `link` (URL string).
`attachment.kind`: `image`, `pdf`, `drawing`, `audio`, `video`, `link`, `table`, `file`, `other`.
`url` is present when the bridge can serve the bytes (images, PDFs, drawings' fallback image,
audio/video/files found on disk). Link cards add `href` and `title`. Scan galleries are
`kind: "image"` with `url` for the first page and `pages: [url, …]`. `modifiedAt` (when known) lets
clients invalidate cached bytes.
`markdown` is only present when `editable` is true; it is the edit format described below.
Locked notes return `locked: true` and `blocks: []`.

`GET /v1/notes/attachments/<attachmentId>` → the raw bytes with a correct `Content-Type`.

### Edit format (plain notes)
A Markdown subset that round-trips through Notes HTML:
- First line `# Title` is the title (Notes' Title style).
- `## ` heading, `### ` subheading, `- ` bullet, `* ` dash list item, `1. ` numbered,
  `> ` quote, fenced ``` blocks → monospace, two-space indent per list level.
- Inline: `**bold**`, `*italic*`, `~~strike~~`, `[text](url)`.
- Blank line between blocks; a single newline inside a body paragraph is a line break.

### Create / update / delete
`POST /v1/notes` body `{"folderId", "markdown"}` → `201` with the full note.
`PUT /v1/notes/<id>` body `{"markdown", "expectedModifiedAt"}` (both required; missing
`expectedModifiedAt` → `400`) → `200` with the full note.
`409 conflict` if `modifiedAt` changed since `expectedModifiedAt`; `422 not_editable` if the note
isn't editable (rich note, locked, in Trash).
`DELETE /v1/notes/<id>` → `204` after a confirmed move to the same account's Recently Deleted
folder, where the note remains recoverable. A note already in Recently Deleted, or an account
without a Recently Deleted folder, answers `422 not_editable`.
An initially missing note answers `404 not_found`.

After the move, the bridge re-reads the note for up to 3 s, until it is in Recently Deleted or no
longer readable (`GET` answers `404` for notes the store hides). If the note is still outside
Recently Deleted at the deadline, it answers `500 internal` with a JSON error. Read errors also
fail the request. On an error, check the note in Notes before retrying; the move may appear later.

The bridge converts the Markdown to the HTML Notes accepts and runs AppleScript
`make new note at folder id … with properties {body: html}` / `set body of note id … to html`.
After a create or update it re-reads the note from the database (retrying up to 3 s for the store to update)
and returns it. If the write succeeded but the re-read didn't catch up, it still answers `200`/`201`
with at least `id` and `modifiedAt: null` so the client never retries a successful write. The
AppleScript itself times out after 45 s; clients allow writes 60 s.
