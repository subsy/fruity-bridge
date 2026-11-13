#!/usr/bin/env python3
"""In-memory mock of the Fruity Bridge API (docs/api.md) for developing the Linux apps.

Run: python3 -I linux/mock/mock_bridge.py [port]   (default 47399)
Then: FRUITY_BRIDGE_URL=http://127.0.0.1:47399 cargo run -p fruity-reminders

MOCK_PARTIAL_WRITES=1 makes note writes answer with just `{"id", "modifiedAt": null}`, as the real
bridge does when its re-read hasn't caught up.
"""
import json
import os
import queue
import re
import sys
import threading
import time
import uuid
from datetime import date, datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse


def now():
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def days_ago(n):
    t = datetime.now(timezone.utc).replace(microsecond=0) - timedelta(days=n)
    return t.isoformat().replace("+00:00", "Z")


PARTIAL_WRITES = os.environ.get("MOCK_PARTIAL_WRITES") == "1"


TODAY = date.today()
LISTS = [
    {"id": "list-shopping", "title": "Shopping", "color": "#FF9500", "account": "iCloud", "editable": True},
    {"id": "list-household", "title": "Household", "color": "#34C759", "account": "iCloud", "editable": True},
    {"id": "list-personal", "title": "Personal", "color": "#007AFF", "account": "iCloud", "editable": True},
    {"id": "list-gifts", "title": "Gifts", "color": "#AF52DE", "account": "iCloud", "editable": True},
    {"id": "list-someday", "title": "Someday", "color": "#8E8E93", "account": "iCloud", "editable": True},
]


def rem(list_id, title, **kw):
    r = {"id": "rem-" + uuid.uuid4().hex[:8], "listId": list_id, "title": title, "notes": None, "url": None,
         "priority": 0, "completed": False, "completedAt": None, "due": None, "recurring": False,
         "recurrenceText": None, "createdAt": now(), "modifiedAt": now()}
    r.update(kw)
    return r


def d(days, time_=None):
    out = {"date": (TODAY + timedelta(days=days)).isoformat()}
    if time_:
        out["time"] = time_
        out["timeZone"] = "Europe/London"
    return out


REMINDERS = [
    rem("list-shopping", "Milk", notes="Semi-skimmed, 2 pints"),
    rem("list-shopping", "Sourdough loaf"),
    rem("list-shopping", "Bananas"),
    rem("list-shopping", "Washing-up liquid", completed=True, completedAt=now()),
    # Outside the 30-day window: only returned with completedSince.
    rem("list-shopping", "Christmas crackers", completed=True, completedAt=days_ago(45)),
    rem("list-household", "Book boiler service", due=d(-1), priority=1),
    rem("list-household", "Put the bins out", due=d(0, "19:30"), recurring=True, recurrenceText="Every week"),
    rem("list-household", "Change smoke alarm batteries", due=d(12)),
    rem("list-personal", "Renew passport", due=d(3), priority=5, notes="Photos in the desk drawer"),
    rem("list-personal", "Reply to Sam about Saturday", due=d(0, "12:00")),
    rem("list-personal", "Call with the New York office", due={"date": (TODAY + timedelta(days=1)).isoformat(),
                                                             "time": "09:00", "timeZone": "America/New_York"}),
    rem("list-personal", "Read the Tailscale serve docs", url="https://tailscale.com/kb/1312/serve"),
    rem("list-gifts", "Lego set for Alex"),
    rem("list-gifts", "Scarf for Mum", priority=9),
    rem("list-someday", "Learn to make croissants"),
]

FOLDERS = [
    {"id": "x-coredata://STORE/ICFolder/p1", "name": "Notes", "account": "iCloud", "parentId": None, "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p2", "name": "Recipes", "account": "iCloud", "parentId": None, "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p3", "name": "Baking", "account": "iCloud", "parentId": "x-coredata://STORE/ICFolder/p2", "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p4", "name": "Travel", "account": "iCloud", "parentId": None, "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p5", "name": "Work", "account": "iCloud", "parentId": None, "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p6", "name": "Meetings", "account": "iCloud", "parentId": "x-coredata://STORE/ICFolder/p5", "isTrash": False, "isSmart": False},
    {"id": "x-coredata://STORE/ICFolder/p9", "name": "Recently Deleted", "account": "iCloud", "parentId": None, "isTrash": True, "isSmart": False},
]

# A 1x1 PNG so attachment fetching can be exercised.
PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000"
    "1f15c4890000000d49444154789c63f8cfc0f01f0005000201a5d1a1a90000000049454e44ae426082")



def _make_png(w, h):
    """A synthetic sunset-over-hills picture, drawn procedurally (no external assets)."""
    import struct
    import zlib
    rows = []
    for y in range(h):
        t = y / h
        row = bytearray([0])
        for x in range(w):
            hill = 0.62 + 0.08 * __import__("math").sin(x / w * 6.3) + 0.04 * __import__("math").sin(x / w * 17)
            dx, dy = x / w - 0.68, t - 0.45
            if t > hill:
                k = (t - hill) / (1 - hill)
                px = (int(46 - 20 * k), int(110 - 50 * k), int(72 - 30 * k))
            elif dx * dx + (dy * h / w) ** 2 < 0.0025:
                px = (255, 214, 120)
            else:
                px = (int(250 - 120 * t), int(170 - 60 * t), int(120 + 90 * t))
            row += bytes(px)
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 6)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", raw) + chunk(b"IEND", b""))


PHOTO = _make_png(640, 360)
# A tiny but valid one-page PDF.
PDF = (b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj "
       b"3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n")
ATTACHMENT_BYTES = {"att-1": (PHOTO, "image/png"), "att-photo": (PHOTO, "image/png"), "att-pdf": (PDF, "application/pdf"),
                    "att-scan-1": (PHOTO, "image/png"), "att-scan-2": (PHOTO, "image/png")}
ATT_MODIFIED = "2026-11-01T10:00:00Z"

NOTES = {}
_pk = [100]


def runs(text, **kw):
    return [dict({"text": text}, **kw)]


def add_note(folder, blocks, markdown=None, pinned=False, minutes_ago=0, locked=False):
    _pk[0] += 1
    nid = f"x-coredata://STORE/ICNote/p{_pk[0]}"
    ts = (datetime.now(timezone.utc) - timedelta(minutes=minutes_ago)).replace(microsecond=0)
    ts = ts.isoformat().replace("+00:00", "Z")
    has_att = any(b["type"] in ("attachment", "table") for b in blocks)
    has_check = any(b["type"] == "checklist" for b in blocks)
    title = "".join(r["text"] for r in blocks[0].get("runs", [])) if blocks else "New Note"
    body_text = " ".join("".join(r["text"] for r in b.get("runs", [])) for b in blocks[1:])
    NOTES[nid] = {
        "id": nid, "folderId": folder, "title": title, "snippet": body_text[:120],
        "createdAt": ts, "modifiedAt": ts, "pinned": pinned, "locked": locked,
        "editable": not (has_att or has_check or locked or folder.endswith("/p9")), "hasAttachments": has_att, "hasChecklist": has_check,
        "blocks": [] if locked else blocks, "markdown": markdown,
    }


add_note("x-coredata://STORE/ICFolder/p1", [
    {"type": "title", "runs": runs("Weekend plan")},
    {"type": "body", "runs": [{"text": "Saturday: "}, {"text": "football at 10", "bold": True}, {"text": ", then lunch at the café."}]},
    {"type": "bullet", "indent": 0, "runs": runs("Pack boots")},
    {"type": "bullet", "indent": 0, "runs": runs("Charge the camera")},
    {"type": "bullet", "indent": 1, "runs": runs("Spare SD card")},
    {"type": "body", "runs": [{"text": "Directions: "}, {"text": "map link", "link": "https://maps.apple.com"}]},
], markdown="# Weekend plan\n\nSaturday: **football at 10**, then lunch at the café.\n\n- Pack boots\n- Charge the camera\n  - Spare SD card\n\nDirections: [map link](https://maps.apple.com)\n", pinned=True, minutes_ago=5)

add_note("x-coredata://STORE/ICFolder/p1", [
    {"type": "title", "runs": runs("Packing list")},
    {"type": "checklist", "indent": 0, "checked": True, "runs": runs("Passports")},
    {"type": "checklist", "indent": 0, "checked": False, "runs": runs("Chargers")},
    {"type": "checklist", "indent": 0, "checked": False, "runs": runs("Swimming kit")},
    {"type": "attachment", "attachment": {"id": "att-1", "kind": "image", "uti": "public.png", "filename": "map.png", "url": "/v1/notes/attachments/att-1", "modifiedAt": ATT_MODIFIED}},
], minutes_ago=90)

add_note("x-coredata://STORE/ICFolder/p3", [
    {"type": "title", "runs": runs("Banana bread")},
    {"type": "heading", "runs": runs("Ingredients")},
    {"type": "numbered", "indent": 0, "number": 1, "runs": runs("3 ripe bananas")},
    {"type": "numbered", "indent": 0, "number": 2, "runs": runs("75 g melted butter")},
    {"type": "numbered", "indent": 0, "number": 3, "runs": runs("150 g sugar")},
    {"type": "heading", "runs": runs("Method")},
    {"type": "body", "runs": runs("Mash, mix, bake at 175 °C for an hour.")},
    {"type": "table", "rows": [["Tin", "Time"], ["Loaf", "60 min"], ["Muffins", "22 min"]]},
], minutes_ago=60 * 24 * 3)

add_note("x-coredata://STORE/ICFolder/p4", [
    {"type": "title", "runs": runs("Wi-Fi at the cottage")},
    {"type": "monospace", "runs": runs("SSID: Seaview\npassword: correct-horse")},
], markdown="# Wi-Fi at the cottage\n\n```\nSSID: Seaview\npassword: correct-horse\n```\n", minutes_ago=60 * 24 * 40)

add_note("x-coredata://STORE/ICFolder/p4", [{"type": "title", "runs": runs("Locked: bank details")}], locked=True, minutes_ago=60 * 24 * 90)

add_note("x-coredata://STORE/ICFolder/p1", [
    {"type": "title", "runs": runs("Reading notes: The Shape of Rivers")},
    {"type": "body", "runs": [{"text": "Started this on the train. A slow, "}, {"text": "generous", "italic": True},
                              {"text": " book about how water finds its way, and how towns grow along it."}]},
    {"type": "heading", "runs": runs("Ideas worth keeping")},
    {"type": "subheading", "runs": runs("On patience")},
    {"type": "body", "runs": [{"text": "Rivers don't hurry, yet they "}, {"text": "always", "underline": True},
                              {"text": " arrive. The author returns to this again and again."}]},
    {"type": "quote", "runs": runs("A river is the report card for its watershed.")},
    {"type": "dash", "indent": 0, "runs": runs("Meanders form where the land is flat")},
    {"type": "dash", "indent": 0, "runs": runs("Oxbow lakes are old meanders cut off")},
    {"type": "dash", "indent": 1, "runs": runs("Look for one on the Wye walk")},
    {"type": "body", "runs": [{"text": "Old title: "}, {"text": "Water Ways", "strikethrough": True},
                              {"text": ". More at "}, {"text": "the publisher's page", "link": "https://example.com/rivers"}, {"text": "."}]},
    {"type": "monospace", "runs": runs("chapter 4, p. 112\nchapter 7, p. 201")},
], minutes_ago=60 * 26)

add_note("x-coredata://STORE/ICFolder/p4", [
    {"type": "title", "runs": runs("Cornwall photos")},
    {"type": "body", "runs": runs("The evening we walked up to the headland.")},
    {"type": "attachment", "attachment": {"id": "att-photo", "kind": "image", "uti": "public.png", "filename": "headland.png", "url": "/v1/notes/attachments/att-photo", "modifiedAt": ATT_MODIFIED}},
    {"type": "body", "runs": runs("Booking confirmation:")},
    {"type": "attachment", "attachment": {"id": "att-pdf", "kind": "pdf", "uti": "com.adobe.pdf", "filename": "Cottage booking.pdf", "url": "/v1/notes/attachments/att-pdf", "modifiedAt": ATT_MODIFIED}},
    {"type": "attachment", "attachment": {"id": "att-audio", "kind": "audio", "uti": "public.mpeg-4-audio", "filename": "Waves.m4a"}},
    {"type": "attachment", "attachment": {"id": "att-link", "kind": "link", "uti": "public.url",
                                          "href": "https://www.nationaltrust.org.uk/visit/cornwall", "title": "Cornwall | National Trust"}},
    {"type": "attachment", "attachment": {"id": "att-scan", "kind": "image", "uti": "com.apple.notes.gallery", "filename": "Scan.png",
                                          "url": "/v1/notes/attachments/att-scan-1", "modifiedAt": ATT_MODIFIED,
                                          "pages": ["/v1/notes/attachments/att-scan-1", "/v1/notes/attachments/att-scan-2"]}},
], minutes_ago=60 * 24 * 12)

add_note("x-coredata://STORE/ICFolder/p6", [
    {"type": "title", "runs": runs("Planning sync")},
    {"type": "body", "runs": runs("Agreed to move the launch to the first week of December.")},
    {"type": "numbered", "indent": 0, "number": 1, "runs": runs("Draft the announcement")},
    {"type": "numbered", "indent": 0, "number": 2, "runs": runs("Check the pricing page")},
], markdown="# Planning sync\n\nAgreed to move the launch to the first week of December.\n\n1. Draft the announcement\n2. Check the pricing page\n", minutes_ago=60 * 5)

add_note("x-coredata://STORE/ICFolder/p1", [
    {"type": "title", "runs": runs("Gift ideas")},
    {"type": "body", "runs": runs("Mum: a good scarf, the blue one from the market. Alex: Lego, anything with trains.")},
], markdown="# Gift ideas\n\nMum: a good scarf, the blue one from the market. Alex: Lego, anything with trains.\n", minutes_ago=60 * 24 * 6)

add_note("x-coredata://STORE/ICFolder/p9", [
    {"type": "title", "runs": runs("Old shopping list")},
    {"type": "body", "runs": runs("Eggs, flour, milk")},
], minutes_ago=60 * 24 * 20)


INLINE = re.compile(r"\*\*(.+?)\*\*|~~(.+?)~~|\*(.+?)\*|\[(.+?)\]\((.+?)\)")


def md_runs(text):
    """Inline Markdown (**bold**, *italic*, ~~strike~~, [text](url)) to runs."""
    out, pos = [], 0
    for m in INLINE.finditer(text):
        if m.start() > pos:
            out.append({"text": text[pos:m.start()]})
        if m.group(1):
            out.append({"text": m.group(1), "bold": True})
        elif m.group(2):
            out.append({"text": m.group(2), "strikethrough": True})
        elif m.group(3):
            out.append({"text": m.group(3), "italic": True})
        else:
            out.append({"text": m.group(4), "link": m.group(5)})
        pos = m.end()
    if pos < len(text) or not out:
        out.append({"text": text[pos:]})
    return out


def md_to_blocks(md):
    runs = md_runs
    blocks = []
    in_code = False
    code = []
    for line in md.splitlines():
        if line.startswith("```"):
            if in_code:
                blocks.append({"type": "monospace", "runs": [{"text": "\n".join(code)}]})
                code = []
            in_code = not in_code
            continue
        if in_code:
            code.append(line)
            continue
        if not line.strip():
            continue
        indent = (len(line) - len(line.lstrip(" "))) // 2
        s = line.strip()
        for prefix, kind in (("# ", "title"), ("## ", "heading"), ("### ", "subheading"), ("> ", "quote")):
            if s.startswith(prefix):
                blocks.append({"type": kind, "runs": runs(s[len(prefix):])})
                break
        else:
            if s.startswith("- "):
                blocks.append({"type": "bullet", "indent": indent, "runs": runs(s[2:])})
            elif s.startswith("* "):
                blocks.append({"type": "dash", "indent": indent, "runs": runs(s[2:])})
            elif s[:1].isdigit() and ". " in s[:5]:
                n, rest = s.split(". ", 1)
                blocks.append({"type": "numbered", "indent": indent, "number": int(n), "runs": runs(rest)})
            else:
                blocks.append({"type": "body", "runs": runs(s)})
    if not blocks or blocks[0]["type"] != "title":
        blocks.insert(0, {"type": "title", "runs": runs("New Note")})
    return blocks


SUBSCRIBERS = []
LOCK = threading.Lock()


def broadcast(kind):
    with LOCK:
        for q in list(SUBSCRIBERS):
            q.put(kind)


def summary(n):
    return {k: v for k, v in n.items() if k not in ("blocks", "markdown")}


def sort_reminders(rs):
    def key(r):
        due = r["due"]
        return (r["completed"], due is None, (due or {}).get("date", ""), (due or {}).get("time", ""), r["createdAt"])
    return sorted(rs, key=key)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("mock: " + fmt % args + "\n")

    def send(self, status, body=None, ctype="application/json"):
        data = b"" if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def err(self, status, code, message, current=None):
        e = {"code": code, "message": message}
        if current is not None:
            e["current"] = current
        self.send(status, {"error": e})

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def route(self):
        u = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        parts = [unquote(p) for p in u.path.split("/") if p]
        return parts, q

    def do_GET(self):
        parts, q = self.route()
        if parts == ["v1", "health"]:
            return self.send(200, {"version": "0.1.0-mock", "reminders": "ok", "notes": "ok", "notesWrite": "ok"})
        if parts == ["v1", "events"]:
            return self.events()
        if parts == ["v1", "reminders", "lists"]:
            counts = q.get("counts", "true") != "false"
            lists = [dict(l, openCount=sum(1 for r in REMINDERS if r["listId"] == l["id"] and not r["completed"]) if counts else None)
                     for l in LISTS]
            return self.send(200, {"lists": lists})
        if len(parts) == 3 and parts[:2] == ["v1", "reminders"]:
            r = next((r for r in REMINDERS if r["id"] == parts[2]), None)
            if not r:
                return self.err(404, "not_found", "That reminder no longer exists.")
            return self.send(200, r)
        if parts == ["v1", "reminders"]:
            # Completed reminders only go back 30 days unless completedSince says otherwise.
            since = q.get("completedSince") or (TODAY - timedelta(days=30)).isoformat()
            rs = [r for r in REMINDERS if not r["completed"] or (r["completedAt"] or "")[:10] >= since]
            if "list" in q:
                rs = [r for r in rs if r["listId"] == q["list"]]
            due = q.get("due")
            if due == "today":
                rs = [r for r in rs if not r["completed"] and r["due"] and r["due"]["date"] <= TODAY.isoformat()]
            elif due == "scheduled":
                rs = [r for r in rs if not r["completed"] and r["due"]]
            else:
                c = q.get("completed", "false")
                if c != "all":
                    rs = [r for r in rs if r["completed"] == (c == "true")]
            return self.send(200, {"reminders": sort_reminders(rs)})
        if parts == ["v1", "notes", "folders"]:
            fs = [dict(f, noteCount=sum(1 for n in NOTES.values() if n["folderId"] == f["id"])) for f in FOLDERS]
            return self.send(200, {"folders": fs})
        if parts == ["v1", "notes"]:
            ns = list(NOTES.values())
            if "folder" in q:
                ns = [n for n in ns if n["folderId"] == q["folder"]]
            elif "q" in q:
                needle = q["q"].lower()
                ns = [n for n in ns if needle in (n["title"] + " " + n["snippet"]).lower()]
            ns.sort(key=lambda n: n["modifiedAt"], reverse=True)
            ns.sort(key=lambda n: not n["pinned"])
            if "recent" in q:
                ns = ns[: int(q["recent"])]
            return self.send(200, {"notes": [summary(n) for n in ns]})
        if len(parts) == 4 and parts[:3] == ["v1", "notes", "attachments"]:
            data, ctype = ATTACHMENT_BYTES.get(parts[3], (PNG, "image/png"))
            return self.send(200, data, ctype)
        if len(parts) == 3 and parts[:2] == ["v1", "notes"]:
            n = NOTES.get(parts[2])
            if not n:
                return self.err(404, "not_found", "That note no longer exists.")
            out = dict(n)
            if not n["editable"]:
                out.pop("markdown", None)
            return self.send(200, out)
        self.err(404, "not_found", "No such endpoint.")

    def do_POST(self):
        parts, _ = self.route()
        b = self.body()
        time.sleep(0.15)
        if parts == ["v1", "reminders"]:
            r = rem(b["listId"], b["title"], notes=b.get("notes"), due=b.get("due"), priority=b.get("priority", 0), url=b.get("url"))
            REMINDERS.append(r)
            broadcast("reminders")
            return self.send(201, r)
        if parts == ["v1", "notes"]:
            blocks = md_to_blocks(b["markdown"])
            add_note(b["folderId"], blocks, markdown=b["markdown"])
            nid = f"x-coredata://STORE/ICNote/p{_pk[0]}"
            broadcast("notes")
            return self.written(201, NOTES[nid])
        self.err(404, "not_found", "No such endpoint.")

    def do_PATCH(self):
        parts, _ = self.route()
        b = self.body()
        time.sleep(0.15)
        if len(parts) == 3 and parts[:2] == ["v1", "reminders"]:
            r = next((r for r in REMINDERS if r["id"] == parts[2]), None)
            if not r:
                return self.err(404, "not_found", "That reminder no longer exists.")
            exp = b.pop("expectedModifiedAt", None)
            if exp and exp != r["modifiedAt"]:
                return self.err(409, "conflict", "This reminder changed on another device.", r)
            for k in ("title", "notes", "due", "priority", "listId", "url"):
                if k in b:
                    r[k] = b[k]
            if "completed" in b:
                r["completed"] = b["completed"]
                r["completedAt"] = now() if b["completed"] else None
            r["modifiedAt"] = now()
            broadcast("reminders")
            return self.send(200, r)
        self.err(404, "not_found", "No such endpoint.")

    def written(self, status, note):
        """A note write's answer: the full note, or just its id when partial writes are on."""
        if PARTIAL_WRITES:
            return self.send(status, {"id": note["id"], "modifiedAt": None})
        return self.send(status, note)

    def do_PUT(self):
        parts, _ = self.route()
        b = self.body()
        time.sleep(0.2)
        if len(parts) == 3 and parts[:2] == ["v1", "notes"]:
            n = NOTES.get(parts[2])
            if not n:
                return self.err(404, "not_found", "That note no longer exists.")
            if not isinstance(b.get("markdown"), str) or not b.get("expectedModifiedAt"):
                return self.err(400, "bad_request", "markdown and expectedModifiedAt are required.")
            if not n["editable"]:
                return self.err(422, "not_editable", "This note has checklists or attachments, so edit it on your iPhone or Mac.")
            if b["expectedModifiedAt"] != n["modifiedAt"]:
                return self.err(409, "conflict", "This note changed on another device.", summary(n))
            n["blocks"] = md_to_blocks(b["markdown"])
            n["markdown"] = b["markdown"]
            n["title"] = "".join(r["text"] for r in n["blocks"][0]["runs"])
            n["snippet"] = " ".join("".join(r["text"] for r in x.get("runs", [])) for x in n["blocks"][1:])[:120]
            n["modifiedAt"] = now()
            broadcast("notes")
            return self.written(200, n)
        self.err(404, "not_found", "No such endpoint.")

    def do_DELETE(self):
        parts, _ = self.route()
        if len(parts) == 3 and parts[:2] == ["v1", "reminders"]:
            REMINDERS[:] = [r for r in REMINDERS if r["id"] != parts[2]]
            broadcast("reminders")
            return self.send(204)
        if len(parts) == 3 and parts[:2] == ["v1", "notes"]:
            n = NOTES.get(parts[2])
            if not n:
                return self.err(404, "not_found", "That note no longer exists.")
            if n["folderId"].endswith("/p9"):
                return self.err(422, "not_editable", "This note is already in Recently Deleted.")
            n["folderId"] = "x-coredata://STORE/ICFolder/p9"
            n["editable"] = False
            broadcast("notes")
            return self.send(204)
        self.err(404, "not_found", "No such endpoint.")

    def events(self):
        q = queue.Queue()
        with LOCK:
            SUBSCRIBERS.append(q)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        try:
            while True:
                try:
                    kind = q.get(timeout=20)
                    msg = f"event: {kind}\ndata: {json.dumps({'changedAt': now()})}\n\n"
                except queue.Empty:
                    msg = ": ping\n\n"
                self.wfile.write(msg.encode())
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            with LOCK:
                SUBSCRIBERS.remove(q)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 47399
    print(f"mock bridge on http://127.0.0.1:{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
