# Fruity Bridge for macOS

Menu-bar app for the always-on Mac. It serves Reminders (EventKit) and Notes (read from the
Notes database, written through AppleScript) on `127.0.0.1:47380`. `tailscale serve` publishes it
at `https://<mac-name>.<tailnet>.ts.net:10000`, adding a path secret and your Tailscale login.
See [the API](../docs/api.md).

## Install or update (on the Mac, or over SSH)

```sh
./make-signing-identity.sh   # once per Mac: stable signature so macOS keeps permissions on updates
./install.sh --start         # build, self-test, install to /Applications, (re)start, tailscale serve
```

`install.sh` without `--start` installs without starting. The LaunchAgent
`~/Library/LaunchAgents/com.plgeek.fruitybridge.plist` starts the app at login and restarts it if it
crashes; choosing Quit from its menu keeps it stopped until the next login or `--start`.
Logs: `~/Library/Logs/Fruity Bridge.log`.

## Permissions (once, over screen sharing)

The app never asks on its own; each permission is requested from its menu-bar icon.

1. **Full Disk Access** (read Notes): menu → *Open Full Disk Access settings…*. Click **+**, choose
   **Applications → Fruity Bridge**, **Open**, and turn it on. Accept *Quit & Reopen*.
2. **Reminders**: menu → *Allow Reminders access…* → **Allow Full Access**.
3. **Notes editing** (Automation): menu → *Allow Notes editing…* → **Allow** in “Fruity Bridge wants
   access to control Notes”. If no dialog appears: System Settings → Privacy & Security →
   Automation → Fruity Bridge → Notes on.

Check from Linux: `curl https://<mac-name>.<tailnet>.ts.net:10000/v1/health` shows `ok` for all
three (`notesWrite` once Notes has been opened).

## Security

Requests must arrive through `tailscale serve` (path secret) with an allowed Tailscale login
(`allowedLogins` in `~/Library/Application Support/Fruity Bridge/config.json`). Browser requests
(`Origin`/`Sec-Fetch-*`) and unknown `Host`s are refused. A process running as your user on the Mac
can still read the secret from `config.json`; such a process could read your Notes anyway.
