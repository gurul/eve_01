#!/usr/bin/env bash
# Ask Muse two questions through the Pi relay, in a fresh side chat: the first answer must hold the word asked for,
# and the second must recall it (Muse kept the conversation). Needs EVE_MUSE_PI=user@host.
set -euo pipefail
: "${EVE_MUSE_PI:?set EVE_MUSE_PI=user@host}"
ASK="/opt/eve-muse/venv-python /opt/eve-muse/ask.py"
SID="$(uuidgen | tr 'A-Z' 'a-z')"
WORD="ZEBRA$RANDOM"

ask() {
    printf '%s' "$1" | ssh -o BatchMode=yes -o ConnectTimeout=6 "$EVE_MUSE_PI" "$ASK --session-id $SID"
}
reply() { python3 -c 'import json,sys; r=json.loads(sys.stdin.readlines()[-1]); assert r.get("ok"), r; print(r["reply"])'; }

first="$(ask "Automated check from eve. Reply with exactly this one word and nothing else: $WORD" | reply)"
echo "first answer: $first"
case "$first" in *"$WORD"*) ;; *) echo "FAIL: the first answer lacks $WORD"; exit 1 ;; esac

second="$(ask "Automated check from eve. Which word did I ask you to reply with a moment ago? Reply with just that word." | reply)"
echo "second answer: $second"
case "$second" in *"$WORD"*) ;; *) echo "FAIL: Muse did not keep the conversation"; exit 1 ;; esac

echo MUSE_ROUNDTRIP_OK
