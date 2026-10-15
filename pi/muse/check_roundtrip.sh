#!/usr/bin/env bash
# Ask Muse two questions through the Pi relay, in a fresh side chat: the first gives it a word to remember and must
# get an answer, and the second must recall the word (Muse kept the conversation). Needs EVE_MUSE_PI=user@host.
# Muse declines a message that tells it to echo a word for a check, so both questions are ordinary requests.
set -euo pipefail
: "${EVE_MUSE_PI:?set EVE_MUSE_PI=user@host}"
ASK="/opt/eve-muse/venv-python /opt/eve-muse/ask.py"
SID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
WORD="ZEBRA$RANDOM"

ask() {
    printf '%s' "$1" | ssh -o BatchMode=yes -o ConnectTimeout=6 "$EVE_MUSE_PI" "$ASK --session-id $SID"
}
reply() { python3 -c 'import json,sys; r=json.loads(sys.stdin.readlines()[-1]); assert r.get("ok"), r; print(r["reply"])'; }

first="$(ask "Just for this conversation, and please do not save it to your memory: my locker code is $WORD." | reply)"
echo "first answer: $first"
[ -n "$first" ] || { echo "FAIL: no first answer"; exit 1; }

second="$(ask "What's my locker code?" | reply)"
echo "second answer: $second"
case "$second" in *"$WORD"*) ;; *) echo "FAIL: Muse did not keep the conversation"; exit 1 ;; esac

echo MUSE_ROUNDTRIP_OK
