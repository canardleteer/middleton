#!/bin/sh
# Local JSON-RPC fixture: plan, build without a report, then a successful nudge.
# Requests are sequential; IDs start at 1 and initialized is a notification.
set -eu
request_id=1
turn_count=0
while IFS= read -r request; do
    is_turn=false
    case "$request" in
        *'"method":"initialize"'*)
            result='{}'
            ;;
        *'"method":"thread/start"'*)
            result='{"approvalPolicy":"on-request","cwd":"/","reasoningEffort":"max","thread":{"id":"test-thread"}}'
            ;;
        *'"method":"turn/start"'*)
            is_turn=true
            turn_count=$((turn_count + 1))
            printf '%s\n' "$request" >> turn-requests.jsonl
            result='{"turn":{"id":"test-turn","status":"inProgress"}}'
            if [ "$turn_count" -eq 3 ]; then
                printf '# Depth\n' > DEPTH.md
            fi
            printf '%s\n' "$turn_count" > turn-count
            ;;
        *) continue ;;
    esac
    printf '{"id":%s,"result":%s}\n' "$request_id" "$result"
    request_id=$((request_id + 1))
    if [ "$is_turn" = true ]; then
        printf '%s\n' '{"method":"turn/completed","params":{"threadId":"test-thread","turn":{"id":"test-turn","status":"completed"}}}'
    fi
done
