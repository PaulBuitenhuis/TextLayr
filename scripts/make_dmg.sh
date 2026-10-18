#!/usr/bin/env bash
#
# Build a drag-to-Applications .dmg for TextLayr.
#
# The mounted image has the app on the left, an "Applications" alias on the
# right, and an arrow between them, so the usual "drag here to install" flow
# just works. Falls back to a plain (but still aliased) image if Finder
# scripting is unavailable.
#
# Usage:
#   scripts/make_dmg.sh <path/to/TextLayr.app> <version> <output.dmg>
set -euo pipefail

app="${1:?usage: make_dmg.sh <app> <version> <out.dmg>}"
version="${2:?usage: make_dmg.sh <app> <version> <out.dmg>}"
out="${3:?usage: make_dmg.sh <app> <version> <out.dmg>}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
background="$repo_root/assets/dmg-background.png"

[[ -d "$app" ]] || { echo "error: no app at $app" >&2; exit 1; }
[[ -f "$background" ]] || { echo "error: no background at $background (run scripts/make_dmg_background.py)" >&2; exit 1; }

volname="TextLayr"
# Window geometry must match scripts/make_dmg_background.py.
win_w=640
win_h=520
win_x=240
win_y=160
app_pos="160, 170"
link_pos="480, 170"
note_pos="320, 385"
note_name="Before you open this.txt"

# A volume keeps its name even if the mount point gets a " 1" suffix. Two disks
# both named "TextLayr" make `tell disk "TextLayr"` ambiguous and the layout lands on
# the wrong one, so make sure the name is free first (this bit us during
# development, leaving images with no .DS_Store at all).
detach_volume() {
    for mp in /Volumes/"$volname" /Volumes/"$volname "\ *; do
        [[ -e "$mp" ]] || continue
        hdiutil detach "$mp" -force -quiet 2>/dev/null || true
    done
}

# `hdiutil detach` transiently returns exit 16 ("Resource busy") while the OS is
# still finished with the volume (Spotlight indexing, Finder flushing
# .DS_Store). Under `set -e` that aborted the whole packaging step on CI, so
# retry a few times and only force-detach as a last resort.
detach_retry() {
    local mp="$1"
    for _ in 1 2 3 4 5; do
        hdiutil detach "$mp" -quiet && return 0
        sleep 2
    done
    hdiutil detach "$mp" -force -quiet || true
}

detach_volume
sleep 1
[[ -e "/Volumes/$volname" ]] && { echo "error: /Volumes/$volname is in use; eject it and retry" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

stage="$work/stage"
mkdir -p "$stage"
# `ditto` rather than `cp -R`: it preserves the bundle's extended attributes and
# therefore its ad-hoc code signature.
ditto "$app" "$stage/$(basename "$app")"
ln -s /Applications "$stage/Applications"
mkdir -p "$stage/.background"
cp "$background" "$stage/.background/background.png"

# The Gatekeeper note rides along in the image: .txt opens without any security
# prompt, so the instruction is there before anyone tries the app.
cat > "$stage/Before you open this.txt" <<'NOTE'
TextLayr is free and open source, so it isn't notarized with a paid Apple
Developer ID. macOS blocks apps like that the first time — this is expected,
and it only happens once.

  1. Drag TextLayr onto the Applications shortcut in this window.
     (Do this first. Opening it straight from the disk image makes macOS run
     a temporary read-only copy, which stops TextLayr updating itself later.)

  2. Open TextLayr from your Applications folder. macOS will refuse.

  3. Click "Done" in the warning — not "Move to Trash".

  4. Open  System Settings -> Privacy & Security  and scroll to "Security".
     Click "Open Anyway" next to the message about TextLayr, enter an admin
     password, then open the app again.
     (The button only appears for about an hour after you tried to open it.)

No button, or you'd rather not? Open Terminal and run:

  xattr -dr com.apple.quarantine /Applications/TextLayr.app

If it says TextLayr "is damaged and can't be opened", that's the same thing
and the same command fixes it.

Prefer to skip all of this entirely? Install from the terminal instead —
files downloaded with curl are never blocked:

  curl -fsSL https://paulbuitenhuis.github.io/TextLayr/install.sh | sh

Full details: https://paulbuitenhuis.github.io/TextLayr/
NOTE

rw="$work/$volname-rw.dmg"
echo "==> staging and building a read-write image"
hdiutil create -quiet -srcfolder "$stage" -volname "$volname" -fs HFS+ \
    -format UDRW -ov "$rw"

echo "==> mounting and laying out the window"
mountpoint="$(hdiutil attach "$rw" -noverify -noautoopen | grep -o '/Volumes/.*' | head -1)"
[[ -n "$mountpoint" ]] || { echo "error: could not mount $rw" >&2; exit 1; }

# Best-effort Finder layout. If Automation permission is denied (or there's no
# GUI session, e.g. some CI), a warning is printed and the image still installs,
# just without the arrow/positions.
layout_ok=0
if osascript <<APPLESCRIPT >/dev/null 2>&1
tell application "Finder"
    tell disk "$volname"
        open
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set the bounds of container window to {$win_x, $win_y, $((win_x + win_w)), $((win_y + win_h))}
        set viewOptions to the icon view options of container window
        set arrangement of viewOptions to not arranged
        set icon size of viewOptions to 128
        set text size of viewOptions to 13
        set background picture of viewOptions to file ".background:background.png"
        set position of item "$(basename "$app")" of container window to {$app_pos}
        set position of item "Applications" of container window to {$link_pos}
        set position of item "$note_name" of container window to {$note_pos}
        update without registering applications
        delay 1
        close
    end tell
end tell
APPLESCRIPT
then
    layout_ok=1
else
    echo "    warning: Finder layout failed (Automation permission?); keeping standard layout" >&2
fi

# Finder flushes .DS_Store when the window closes / the disk is ejected.
sync
sleep 1
if [[ $layout_ok -eq 1 && ! -f "$mountpoint/.DS_Store" ]]; then
    echo "    warning: no .DS_Store after layout; Finder may not have applied it" >&2
    layout_ok=0
fi
detach_retry "$mountpoint"

echo "==> compressing"
rm -f "$out"
hdiutil convert "$rw" -quiet -format UDZO -imagekey zlib-level=9 -o "$out"

# Verify the compressed image actually carries the layout.
verify_mp="$(hdiutil attach "$out" -nobrowse -readonly | grep -o '/Volumes/.*' | head -1)"
if [[ -n "$verify_mp" ]]; then
    if [[ -f "$verify_mp/.DS_Store" ]]; then
        echo "==> verified: layout (.DS_Store) baked in"
    else
        echo "==> warning: .DS_Store missing from the final image — no arrow/positions" >&2
    fi
    detach_retry "$verify_mp"
fi

echo "==> done: $out"
