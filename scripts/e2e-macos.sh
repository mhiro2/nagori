#!/usr/bin/env bash
# Smoke-test the macOS clipboard pipeline end-to-end against a freshly built
# `nagori` daemon. Drives a real `NSPasteboard` via `pbcopy` / `pbpaste` so the
# `MacosClipboard` capture path, IPC, storage, search, and copy-back all run
# the same code the desktop app uses.
#
# Usage:
#   scripts/e2e-macos.sh
#   NAGORI_E2E_BIN=/path/to/nagori scripts/e2e-macos.sh
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "e2e-macos.sh: this script is macOS-only" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${NAGORI_E2E_BIN:-${REPO_ROOT}/target/release/nagori}"
if [[ ! -x "${BIN}" ]]; then
  echo "e2e-macos.sh: nagori binary not found at ${BIN}" >&2
  echo "  hint: cargo build --release -p nagori-cli" >&2
  exit 2
fi

WORK_DIR="$(mktemp -d -t nagori-e2e.XXXXXX)"
SOCKET="${WORK_DIR}/nagori.sock"
DB="${WORK_DIR}/nagori.sqlite"
DAEMON_LOG="${WORK_DIR}/daemon.log"
CLI_ERR="${WORK_DIR}/cli.err"
RESTORE_CLIPBOARD="${WORK_DIR}/clipboard.bak"
DAEMON_PID=""
CLIPBOARD_SAVED=0

# Re-root HOME so `dirs::data_local_dir()` (used by both daemon and CLI for
# the IPC auth-token file) resolves into the temp work dir. Without this, a
# local run would clobber `~/Library/Application Support/nagori/nagori.token`
# and break any nagori daemon/CLI session the developer already has open.
export HOME="${WORK_DIR}/home"
mkdir -p "${HOME}/Library/Application Support/nagori"

# Save the user's current clipboard so a local run does not nuke it.
if pbpaste > "${RESTORE_CLIPBOARD}" 2>/dev/null; then
  CLIPBOARD_SAVED=1
fi

cleanup() {
  local rc=$?
  if [[ -n "${DAEMON_PID}" ]] && kill -0 "${DAEMON_PID}" 2>/dev/null; then
    kill "${DAEMON_PID}" 2>/dev/null || true
    wait "${DAEMON_PID}" 2>/dev/null || true
  fi
  if [[ "${CI:-}" != "true" ]] && (( CLIPBOARD_SAVED == 1 )); then
    pbcopy < "${RESTORE_CLIPBOARD}" 2>/dev/null || true
  fi
  if [[ ${rc} -ne 0 ]]; then
    echo "::group::daemon log (${DAEMON_LOG})"
    cat "${DAEMON_LOG}" 2>/dev/null || true
    echo "::endgroup::"
    if [[ -s "${CLI_ERR}" ]]; then
      echo "::group::last cli stderr (${CLI_ERR})"
      cat "${CLI_ERR}"
      echo "::endgroup::"
    fi
  fi
  rm -rf "${WORK_DIR}"
  exit ${rc}
}
trap cleanup EXIT

step() { printf "\n--- %s ---\n" "$*"; }

run_cli() { "${BIN}" --ipc "${SOCKET}" "$@"; }

wait_for() {
  local desc="$1"; shift
  local deadline=$(( $(date +%s) + 30 ))
  while (( $(date +%s) < deadline )); do
    if "$@"; then return 0; fi
    sleep 0.2
  done
  echo "timeout waiting for ${desc}" >&2
  return 1
}

step "start daemon"
# A freshly built `target/release/nagori` does not have Accessibility
# permission, so the AX-based "frontmost focus is a secure text field"
# probe fails every tick. After three consecutive failures the capture
# loop flips to fail-closed and silently drops user-issued pbcopy events,
# breaking every test below the first one. Opt out for the harness; the
# bundle-id override list still fires.
NAGORI_DISABLE_SECURE_FOCUS_FAIL_CLOSED=1 "${BIN}" \
  --ipc "${SOCKET}" \
  --db "${DB}" \
  daemon run \
  --capture-interval-ms 200 \
  --maintenance-interval-min 60 \
  > "${DAEMON_LOG}" 2>&1 &
DAEMON_PID=$!

wait_for "ipc socket" test -S "${SOCKET}"
wait_for "daemon health" run_cli daemon status >/dev/null

step "capture: pbcopy -> daemon -> nagori list"
MARKER="nagori e2e marker $(date -u +%Y%m%dT%H%M%SZ) ${RANDOM}${RANDOM}"
printf %s "${MARKER}" | pbcopy

# Capture loop polls every 200ms; give it a generous budget under CI load.
# Keep the latest CLI stderr around so a token/socket/JSON failure surfaces
# in the cleanup diagnostics instead of disappearing behind `|| true`.
ENTRY_JSON=""
ENTRY_ID=""
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if ENTRY_JSON="$(run_cli list --limit 1 --json 2> "${CLI_ERR}")"; then
    if [[ -n "${ENTRY_JSON}" ]] \
      && [[ "$(printf %s "${ENTRY_JSON}" | jq -r '.[0].text // .[0].preview // ""')" == "${MARKER}" ]]; then
      ENTRY_ID="$(printf %s "${ENTRY_JSON}" | jq -r '.[0].id')"
      break
    fi
  fi
  sleep 0.2
done

if [[ -z "${ENTRY_ID}" ]]; then
  echo "capture failed; latest entry was:" >&2
  echo "${ENTRY_JSON}" >&2
  exit 1
fi
echo "captured id=${ENTRY_ID}"

SENSITIVITY="$(printf %s "${ENTRY_JSON}" | jq -r '.[0].sensitivity')"
if [[ "${SENSITIVITY}" != "Public" ]]; then
  echo "expected Public sensitivity, got ${SENSITIVITY}" >&2
  exit 1
fi

step "search: full-text hits the captured entry"
SEARCH_HITS="$(run_cli search 'nagori e2e marker' --limit 5 --json | jq --arg id "${ENTRY_ID}" '[.[] | select(.id == $id)] | length')"
if [[ "${SEARCH_HITS}" != "1" ]]; then
  echo "search did not return the captured entry (hits=${SEARCH_HITS})" >&2
  run_cli search 'nagori e2e marker' --limit 5 --json >&2 || true
  exit 1
fi

step "copy: nagori copy -> pbpaste returns the original text"
# Overwrite the pasteboard with a sentinel so a no-op `copy` would be visible.
printf %s "sentinel-not-the-marker" | pbcopy
run_cli copy "${ENTRY_ID}" >/dev/null

PASTED=""
deadline=$(( $(date +%s) + 5 ))
while (( $(date +%s) < deadline )); do
  PASTED="$(pbpaste)"
  [[ "${PASTED}" == "${MARKER}" ]] && break
  sleep 0.1
done
if [[ "${PASTED}" != "${MARKER}" ]]; then
  echo "pbpaste did not return the marker after copy" >&2
  echo "  expected: ${MARKER}" >&2
  echo "  actual:   ${PASTED}" >&2
  exit 1
fi

step "pin / unpin round-trip"
run_cli pin "${ENTRY_ID}" >/dev/null
PINNED_HITS="$(run_cli list --pinned --json | jq --arg id "${ENTRY_ID}" '[.[] | select(.id == $id)] | length')"
if [[ "${PINNED_HITS}" != "1" ]]; then
  echo "pinned list did not contain the entry" >&2
  exit 1
fi
run_cli unpin "${ENTRY_ID}" >/dev/null
PINNED_HITS_AFTER="$(run_cli list --pinned --json | jq --arg id "${ENTRY_ID}" '[.[] | select(.id == $id)] | length')"
if [[ "${PINNED_HITS_AFTER}" != "0" ]]; then
  echo "unpin did not remove the entry from pinned list" >&2
  exit 1
fi

step "delete tombstones the entry"
run_cli delete "${ENTRY_ID}" >/dev/null
REMAINING="$(run_cli list --limit 50 --json | jq --arg id "${ENTRY_ID}" '[.[] | select(.id == $id)] | length')"
if [[ "${REMAINING}" != "0" ]]; then
  echo "deleted entry still present in list" >&2
  exit 1
fi

# Each pbcopy bumps NSPasteboard changeCount, so the daemon stores them as
# distinct entries (key: "nspb:<changeCount>") even when text repeats. The
# capture loop only sees whichever value happens to be on the pasteboard at
# poll time though, so push markers one at a time and confirm each one has
# landed before pushing the next; otherwise CI scheduling jitter could
# silently drop intermediate markers.
push_and_wait() {
  local marker="$1"
  printf %s "${marker}" | pbcopy
  local deadline=$(( $(date +%s) + 10 ))
  while (( $(date +%s) < deadline )); do
    if run_cli list --limit 1 --json 2> "${CLI_ERR}" \
      | jq -e --arg t "${marker}" '.[0] | (.text // .preview) == $t' >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "marker did not land at top of list: ${marker}" >&2
  return 1
}

step "multi-copy ordering newest-first"
ORDER_SUFFIX="$(date -u +%Y%m%dT%H%M%SZ)-${RANDOM}${RANDOM}"
MARKER_A="nagori e2e order A ${ORDER_SUFFIX}"
MARKER_B="nagori e2e order B ${ORDER_SUFFIX}"
MARKER_C="nagori e2e order C ${ORDER_SUFFIX}"
push_and_wait "${MARKER_A}"
push_and_wait "${MARKER_B}"
push_and_wait "${MARKER_C}"

ORDER_JSON=""
TOP3=""
EXPECTED_TOP3="${MARKER_C}"$'\t'"${MARKER_B}"$'\t'"${MARKER_A}"
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if ORDER_JSON="$(run_cli list --limit 5 --json 2> "${CLI_ERR}")"; then
    TOP3="$(printf %s "${ORDER_JSON}" | jq -r '.[0:3] | map(.text // .preview // "") | @tsv')"
    [[ "${TOP3}" == "${EXPECTED_TOP3}" ]] && break
  fi
  sleep 0.2
done
if [[ "${TOP3}" != "${EXPECTED_TOP3}" ]]; then
  echo "ordering check failed; top 3 entries were:" >&2
  printf %s "${ORDER_JSON}" | jq -r '.[0:3] | map(.text // .preview // "")' >&2 || true
  exit 1
fi

step "copy back the oldest of the three"
ENTRY_A="$(printf %s "${ORDER_JSON}" | jq --arg t "${MARKER_A}" -r 'first(.[] | select((.text // .preview) == $t)) | .id')"
if [[ -z "${ENTRY_A}" || "${ENTRY_A}" == "null" ]]; then
  echo "could not resolve id for marker A" >&2
  exit 1
fi
printf %s "sentinel-not-marker-A" | pbcopy
run_cli copy "${ENTRY_A}" >/dev/null

PASTED=""
deadline=$(( $(date +%s) + 5 ))
while (( $(date +%s) < deadline )); do
  PASTED="$(pbpaste)"
  [[ "${PASTED}" == "${MARKER_A}" ]] && break
  sleep 0.1
done
if [[ "${PASTED}" != "${MARKER_A}" ]]; then
  echo "older-entry copy-back did not return marker A" >&2
  echo "  expected: ${MARKER_A}" >&2
  echo "  actual:   ${PASTED}" >&2
  exit 1
fi

# AppleScript's «class PNGf» tags the data as NSPasteboardTypePNG, which is
# exactly what the macOS capture path reads first (see clipboard.rs).
step "PNG capture"
PNG="${WORK_DIR}/sample.png"
# Smallest valid 1x1 transparent PNG, base64-encoded inline.
printf %s 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==' \
  | base64 -D > "${PNG}"
osascript -e "set the clipboard to (read (POSIX file \"${PNG}\") as «class PNGf»)"

IMG_KIND=""
IMG_SENS=""
IMG_JSON=""
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if IMG_JSON="$(run_cli list --limit 1 --json 2> "${CLI_ERR}")"; then
    IMG_KIND="$(printf %s "${IMG_JSON}" | jq -r '.[0].kind // ""')"
    IMG_SENS="$(printf %s "${IMG_JSON}" | jq -r '.[0].sensitivity // ""')"
    [[ "${IMG_KIND}" == "Image" ]] && break
  fi
  sleep 0.2
done
if [[ "${IMG_KIND}" != "Image" ]]; then
  echo "image capture failed; latest entry kind=${IMG_KIND}" >&2
  echo "${IMG_JSON}" >&2
  exit 1
fi
if [[ "${IMG_SENS}" != "Public" ]]; then
  echo "expected Public sensitivity for image, got ${IMG_SENS}" >&2
  exit 1
fi

# AppKit's `setData_forType:` keeps the bytes verbatim (no PNG re-encode), so
# `nagori copy <png_id>` should put the original 1x1 fixture back on the
# pasteboard byte-for-byte. We overwrite with a sentinel first so a no-op
# would surface as a stuck non-image clipboard.
step "image copy-back: nagori copy <png_id> -> osascript reads PNG bytes"
IMG_ID="$(printf %s "${IMG_JSON}" | jq -r '.[0].id // ""')"
if [[ -z "${IMG_ID}" || "${IMG_ID}" == "null" ]]; then
  echo "could not resolve id for the captured PNG entry" >&2
  exit 1
fi

printf %s "sentinel-not-an-image" | pbcopy
run_cli copy "${IMG_ID}" >/dev/null

OUT_PNG="${WORK_DIR}/copied.png"
got_png=0
deadline=$(( $(date +%s) + 5 ))
while (( $(date +%s) < deadline )); do
  rm -f "${OUT_PNG}"
  if osascript \
      -e "set pngData to (the clipboard as «class PNGf»)" \
      -e "set fp to (POSIX file \"${OUT_PNG}\")" \
      -e "set fh to (open for access fp with write permission)" \
      -e "set eof fh to 0" \
      -e "write pngData to fh" \
      -e "close access fh" >/dev/null 2>&1 \
    && [[ -s "${OUT_PNG}" ]]
  then
    got_png=1
    break
  fi
  sleep 0.1
done
if (( got_png != 1 )); then
  echo "image copy-back: clipboard did not expose PNG bytes after 'nagori copy'" >&2
  exit 1
fi

MAGIC="$(head -c 8 "${OUT_PNG}" | xxd -p)"
if [[ "${MAGIC}" != "89504e470d0a1a0a" ]]; then
  echo "image copy-back: PNG magic mismatch (got ${MAGIC})" >&2
  exit 1
fi
if ! cmp -s "${PNG}" "${OUT_PNG}"; then
  ORIG_SUM="$(shasum -a 256 "${PNG}" | awk '{print $1}')"
  COPY_SUM="$(shasum -a 256 "${OUT_PNG}" | awk '{print $1}')"
  echo "image copy-back: bytes differ from the captured PNG fixture" >&2
  echo "  original sha256=${ORIG_SUM} size=$(stat -f%z "${PNG}")" >&2
  echo "  copied   sha256=${COPY_SUM} size=$(stat -f%z "${OUT_PNG}")" >&2
  exit 1
fi

step "file-list round-trip via NSPasteboardTypeFileURL"
# Include a space in the filename so the daemon's `Url::from_file_path` /
# `Url::parse` percent-encoding round-trip on macOS is exercised end-to-end
# (mirrors the Linux e2e's same-shaped check). The macOS capture path
# enumerates `pasteboardItems()` and reads `NSPasteboardTypeFileURL` per item,
# which is what AppleScript's «class furl» publishes when you set the
# clipboard to a `POSIX file`.
URI_FILE="${WORK_DIR}/file url-a.txt"
printf "macos-furl-content" > "${URI_FILE}"
# No pre-step sentinel: the previous "image copy-back" step already pushed a
# distinct change-count onto the pasteboard, and writing a file URL via
# NSPasteboard will land its own change-count regardless of the prior slot
# type. The copy-back sentinel below is what actually proves `nagori copy`
# was not a no-op. Drive AppKit directly from Swift instead of
# `osascript "set the clipboard to (POSIX file ...)"` because the
# AppleScript path goes Apple Events -> System Events -> pasteboard, which
# has been observed to race the daemon's capture loop on macos-26 runners.
PUSH_FURL_SWIFT="${WORK_DIR}/push_furl.swift"
cat > "${PUSH_FURL_SWIFT}" <<'SWIFT'
import AppKit
guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("expected <path> arg\n".utf8))
    exit(2)
}
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let pb = NSPasteboard.general
pb.clearContents()
exit(pb.writeObjects([url as NSURL]) ? 0 : 1)
SWIFT
if ! swift "${PUSH_FURL_SWIFT}" "${URI_FILE}" >/dev/null 2>&1; then
  echo "failed to push file URL onto NSPasteboard via swift" >&2
  exit 1
fi

FURL_ENTRY_ID=""
FURL_LIST_JSON=""
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if FURL_LIST_JSON="$(run_cli list --limit 1 --json 2> "${CLI_ERR}")"; then
    KIND="$(printf %s "${FURL_LIST_JSON}" | jq -r '.[0].kind // ""')"
    HAS_URI="$(printf %s "${FURL_LIST_JSON}" \
      | jq -r '[.[0].representation_summary[] | select(.mime_type == "text/uri-list")] | length')"
    if [[ "${KIND}" == "FileList" && "${HAS_URI}" == "1" ]]; then
      FURL_ENTRY_ID="$(printf %s "${FURL_LIST_JSON}" | jq -r '.[0].id')"
      break
    fi
  fi
  sleep 0.2
done
if [[ -z "${FURL_ENTRY_ID}" ]]; then
  echo "file-list capture failed; latest entry was:" >&2
  printf %s "${FURL_LIST_JSON}" >&2 || true
  exit 1
fi
echo "captured file-list id=${FURL_ENTRY_ID}"

# Overwrite with plain text so a no-op `nagori copy` would be visible: an
# AppleScript "as «class furl»" coercion on a text-only pasteboard raises an
# error, which would propagate through `osascript` as a non-zero exit.
printf %s "sentinel-not-a-furl" | pbcopy
run_cli copy "${FURL_ENTRY_ID}" >/dev/null

PASTED_FURL=""
deadline=$(( $(date +%s) + 5 ))
while (( $(date +%s) < deadline )); do
  PASTED_FURL="$(osascript -e 'POSIX path of (the clipboard as «class furl»)' 2>/dev/null || true)"
  [[ "${PASTED_FURL}" == "${URI_FILE}" ]] && break
  sleep 0.1
done
if [[ "${PASTED_FURL}" != "${URI_FILE}" ]]; then
  echo "file-list copy-back did not republish the file URL" >&2
  echo "  expected: ${URI_FILE}" >&2
  echo "  actual:   ${PASTED_FURL}" >&2
  exit 1
fi

step "multi-representation preserve round-trip (HTML + plain)"
# Push HTML + plain text together on the same NSPasteboard item so the
# daemon's macOS capture sees both reps on one snapshot:
#   - `arboard::Clipboard::get_text()` reads NSPasteboardTypeString
#   - `collect_macos_extras` adds NSPasteboardTypeHTML
# The resulting entry must carry both `text/html` and `text/plain` in
# `representation_summary`. The copy-back assertion below proves
# `write_representations` republishes the full set instead of collapsing
# to a single rep on the way out. Drive AppKit directly from Swift because
# `osascript`'s clipboard model is single-coercion (`set the clipboard to
# ...` overwrites all types on the pasteboard), so it can't compose two
# pasteboard types in one snapshot.
MULTI_SUFFIX="$(date -u +%Y%m%dT%H%M%SZ)-${RANDOM}${RANDOM}"
MULTI_TEXT="multi-rep marker ${MULTI_SUFFIX}"
MULTI_HTML="<p>multi-rep <b>marker</b> ${MULTI_SUFFIX}</p>"
PUSH_MULTI_SWIFT="${WORK_DIR}/push_multi.swift"
cat > "${PUSH_MULTI_SWIFT}" <<'SWIFT'
import AppKit
guard CommandLine.arguments.count == 3 else {
    FileHandle.standardError.write(Data("expected <html> <plain> args\n".utf8))
    exit(2)
}
let pb = NSPasteboard.general
pb.clearContents()
let html = CommandLine.arguments[1]
let plain = CommandLine.arguments[2]
let okHtml = pb.setString(html, forType: .html)
let okString = pb.setString(plain, forType: .string)
exit(okHtml && okString ? 0 : 1)
SWIFT
if ! swift "${PUSH_MULTI_SWIFT}" "${MULTI_HTML}" "${MULTI_TEXT}" >/dev/null 2>&1; then
  echo "failed to push HTML+plain onto NSPasteboard via swift" >&2
  exit 1
fi

MULTI_ENTRY_ID=""
MULTI_LIST_JSON=""
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if MULTI_LIST_JSON="$(run_cli list --limit 1 --json 2> "${CLI_ERR}")"; then
    HAS_HTML="$(printf %s "${MULTI_LIST_JSON}" \
      | jq -r '[.[0].representation_summary[] | select(.mime_type == "text/html")] | length')"
    HAS_TEXT="$(printf %s "${MULTI_LIST_JSON}" \
      | jq -r '[.[0].representation_summary[] | select(.mime_type == "text/plain")] | length')"
    if [[ "${HAS_HTML}" == "1" && "${HAS_TEXT}" == "1" ]]; then
      MULTI_ENTRY_ID="$(printf %s "${MULTI_LIST_JSON}" | jq -r '.[0].id')"
      break
    fi
  fi
  sleep 0.2
done
if [[ -z "${MULTI_ENTRY_ID}" ]]; then
  echo "multi-rep capture failed; latest entry was:" >&2
  printf %s "${MULTI_LIST_JSON}" >&2 || true
  exit 1
fi
echo "captured multi-rep id=${MULTI_ENTRY_ID}"

# Sentinel overwrite: pbcopy wipes every prior type on the pasteboard, so a
# no-op `nagori copy` would surface as the html-read-back returning the
# sentinel (or empty) instead of our HTML marker.
printf %s "sentinel-pre-multi-rep" | pbcopy
run_cli copy "${MULTI_ENTRY_ID}" >/dev/null

READ_MULTI_SWIFT="${WORK_DIR}/read_multi.swift"
# Compare both reps inside Swift so an AppKit-wrapped HTML payload (which
# can grow extra `<meta>` headers or split across lines on some macOS
# versions) is still matched correctly via Swift's `String.contains`. A
# shell `awk` pass over `print`-separated output only captures the first
# line of each rep and would falsely fail when the marker ends up on a
# wrapped continuation line.
cat > "${READ_MULTI_SWIFT}" <<'SWIFT'
import AppKit
let args = Array(CommandLine.arguments.dropFirst())
guard args.count == 2 else {
    FileHandle.standardError.write(Data("expected <html-suffix> <plain> args\n".utf8))
    exit(2)
}
let expectedSuffix = args[0]
let expectedPlain = args[1]
let pb = NSPasteboard.general
let html = pb.string(forType: .html) ?? ""
let plain = pb.string(forType: .string) ?? ""
let htmlOk = html.contains(expectedSuffix)
let plainOk = plain == expectedPlain
if htmlOk && plainOk {
    print("OK")
    exit(0)
}
print("htmlOk=\(htmlOk) plainOk=\(plainOk)")
print("html=\(html.prefix(400))")
print("plain=\(plain.prefix(400))")
exit(1)
SWIFT
# Allow up to 15s because the first `swift` invocation has to JIT-compile
# the script (typically 2–4s on macOS runners) before reading the
# pasteboard. The daemon writes back synchronously before `nagori copy`
# returns, so the data is already there — the loop is only here to absorb
# the Swift startup cost without flaking under CI scheduling jitter.
MULTI_READ_LOG="${WORK_DIR}/multi-readback.log"
MULTI_OK=0
deadline=$(( $(date +%s) + 15 ))
while (( $(date +%s) < deadline )); do
  if swift "${READ_MULTI_SWIFT}" "${MULTI_SUFFIX}" "${MULTI_TEXT}" \
       > "${MULTI_READ_LOG}" 2>&1; then
    MULTI_OK=1
    break
  fi
  sleep 0.1
done
if (( MULTI_OK != 1 )); then
  echo "multi-rep copy-back did not republish both types" >&2
  cat "${MULTI_READ_LOG}" >&2 || true
  exit 1
fi

step "graceful shutdown via daemon stop"
run_cli daemon stop >/dev/null
# Wait for the background process to exit on its own; do not force-kill.
for _ in $(seq 1 50); do
  kill -0 "${DAEMON_PID}" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "${DAEMON_PID}" 2>/dev/null; then
  echo "daemon did not exit after 'nagori daemon stop'" >&2
  exit 1
fi
DAEMON_PID=""

echo "e2e ok"
