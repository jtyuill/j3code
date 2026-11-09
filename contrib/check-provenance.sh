#!/usr/bin/env bash
# Fails if a tracked file names the project j3code was derived from or cites
# its sources (docs/m8.md, "Gates"). docs/m8.md records that history and
# is exempt; a line may opt out with the marker "deny-list: allow".
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

identity='t3code|t3tools|pingdotgg|\bt3\b|x-t3-|t3-context|well-known/t3|T3[A-Z][a-z]'
citations='OV2:|C/[a-zA-Z]+\.ts|CR/|Sidebar\.logic|threadSort\.ts|threadSettled\.ts|Orchestrator\.ts'

check() {
    local name=$1 flags=$2 pattern=$3
    local hits
    hits=$(git grep -I -n $flags -E "$pattern" -- . \
        ':!docs/m8.md' ':!contrib/check-provenance.sh' | grep -v 'deny-list: allow' || true)
    if [[ -n $hits ]]; then
        echo "$name:"
        echo "$hits"
        return 1
    fi
}

status=0
check "identity" -i "$identity" || status=1
check "citations" "" "$citations" || status=1
exit $status
