#!/bin/bash
# Installs Fruity Bridge on the Mac: builds and self-tests the app, installs it to
# /Applications, writes the LaunchAgent and publishes the bridge with `tailscale serve`.
#
#   ./install.sh          install, but don't start anything (first install / permission session)
#   ./install.sh --start  also load the LaunchAgent (or restart the app if it's already loaded)
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then
  echo "Run this on the Mac that should serve Reminders and Notes." >&2
  exit 1
fi
start=false
[[ "${1:-}" == "--start" ]] && start=true

source_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
label="com.plgeek.fruitybridge"
app_dir="/Applications"
app="$app_dir/Fruity Bridge.app"
binary="$app/Contents/MacOS/FruityBridge"
agent="$HOME/Library/LaunchAgents/$label.plist"
log="$HOME/Library/Logs/Fruity Bridge.log"
tailscale="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
port=47380
https_port=10000

build="$(mktemp -d "${TMPDIR:-/tmp}/fruity-bridge-install.XXXXXX")"
trap 'rm -rf "$build"' EXIT
"$source_dir/build.sh" "$build"

loaded=false
launchctl print "gui/$(id -u)/$label" >/dev/null 2>&1 && loaded=true
if pgrep -fx "$binary" >/dev/null && ! $loaded; then
  echo "Fruity Bridge is running outside launchd; quit it from its menu first." >&2
  exit 1
fi

mkdir -p "$app_dir"
rm -rf "$app.new"
ditto "$build/Fruity Bridge.app" "$app.new"
rm -rf "$app"
mv "$app.new" "$app"
codesign --verify --strict "$app"
echo "Installed $app"

mkdir -p "$(dirname "$agent")" "$(dirname "$log")"
cat > "$agent" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$label</string>
	<key>ProgramArguments</key>
	<array>
		<string>$binary</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>StandardOutPath</key>
	<string>$log</string>
	<key>StandardErrorPath</key>
	<string>$log</string>
</dict>
</plist>
PLIST
plutil -lint "$agent" >/dev/null
echo "Wrote $agent"

# Publish 127.0.0.1:$port on https://<this-mac>.<tailnet>.ts.net:$https_port (tailnet only).
# serve prepends the random path secret from config.json to every request it forwards; the bridge
# trusts Tailscale-User-Login only on requests that carry it, so local processes can't forge it.
secret="$("$binary" --path-secret)"
if [[ ! "$secret" =~ ^[0-9a-f]{48}$ ]]; then
  echo "Couldn't read the bridge's path secret." >&2
  exit 1
fi
if [[ -x "$tailscale" ]]; then
  target="http://127.0.0.1:$port/$secret"
  if "$tailscale" serve status --json 2>/dev/null | grep -qF "\"Proxy\": \"$target\""; then
    echo "tailscale serve already publishes the bridge on :$https_port"
  else
    # Replaces only the :$https_port mapping; other serve ports are left alone.
    "$tailscale" serve --bg --yes --https="$https_port" "$target" >/dev/null
    echo "tailscale serve now publishes the bridge on :$https_port"
  fi
  status_json="$("$tailscale" status --json 2>/dev/null || true)"
  field() { plutil -extract "$1" raw -o - - <<<"$status_json" 2>/dev/null || true; }
  dns_name="$(field Self.DNSName)"
  dns_name="${dns_name%.}"
  if [[ -n "$dns_name" ]]; then
    "$binary" --set-tailnet-url "https://$dns_name:$https_port"
  fi
  # Allow the login this Mac's Tailscale is signed in with; add others with --allow-login.
  # Tagged Macs have no user login to allow.
  user_id="$(field Self.UserID)"
  login=""
  if [[ -n "$user_id" ]]; then login="$(field "User.$user_id.LoginName")"; fi
  [[ "$login" == "tagged-devices" ]] && login=""
  if [[ -n "$login" ]]; then
    "$binary" --allow-login "$login"
  else
    echo "Couldn't read this Mac's Tailscale login; allow one with: \"$binary\" --allow-login you@example.com" >&2
  fi
else
  echo "Tailscale CLI not found at $tailscale; publish the bridge with: tailscale serve --bg --https=$https_port http://127.0.0.1:$port/<pathSecret from config.json>" >&2
fi

if $start; then
  if $loaded; then
    launchctl kickstart -k "gui/$(id -u)/$label"
    echo "Restarted Fruity Bridge."
  else
    launchctl bootstrap "gui/$(id -u)" "$agent"
    echo "Started Fruity Bridge."
  fi
elif $loaded; then
  launchctl kickstart -k "gui/$(id -u)/$label"
  echo "Restarted the running Fruity Bridge with the new build."
else
  echo "Not started. In the Mac's GUI session, start it with:"
  echo "  launchctl bootstrap gui/\$(id -u) \"$agent\""
fi
