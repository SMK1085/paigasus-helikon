#!/usr/bin/env bash
# SMA-840 live probe: Anthropic extended thinking + tool loop wire format and round trip.
# Reads ANTHROPIC_API_KEY from the environment. Never prints headers, the key or signature values.
# Usage: bash 2026-11-21-sma-840-anthropic-thinking-probe.sh
#   The default model is claude-sonnet-4-5, which accepts the default enabled thinking.
#   Optional: MODEL=..., MODEL_2=..., THINKING='{"type":"enabled","budget_tokens":2048}',
#   BETA=interleaved-thinking-2025-05-14.
#   claude-sonnet-5-5 rejects enabled thinking with HTTP 400. For that model, set
#   MODEL=claude-sonnet-5-5 THINKING='{"type":"adaptive"}'.
set -uo pipefail

: "${ANTHROPIC_API_KEY:?set ANTHROPIC_API_KEY}"
MODEL="${MODEL:-claude-sonnet-4-5}"
DEFAULT_THINKING='{"type":"enabled","budget_tokens":2048}'
THINKING="${THINKING:-$DEFAULT_THINKING}"
BETA="${BETA-interleaved-thinking-2025-05-14}"
EXTRA="${EXTRA:-{\}}"
OUT="${OUT:-$(cd "$(dirname "$0")" && pwd)/probe-840-out}"
mkdir -p "$OUT"
REDACT_TRIGGER="ANTHROPIC_MAGIC_STRING_TRIGGER_REDACTED_THINKING_46C9A13E193C177646C7398A98432ECCCE4C1253D5E2D82641AC0E52CC2876CB"

TOOLS='[
 {"name":"lookup_city_code","description":"Return the internal code for a city. You need the code before you can get the weather.","input_schema":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}},
 {"name":"weather_by_code","description":"Get the weather for an internal city code.","input_schema":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}}
]'
BASE_PROMPT="What is the weather in Paris? First look up the city code, then get the weather with that code. Think briefly before each step."
PROMPT="${PROMPT:-$BASE_PROMPT}"

call() { # $1=body file, $2=out file, $3=model -> prints HTTP code
  local hdr=(-H "x-api-key: ${ANTHROPIC_API_KEY}" -H "anthropic-version: 2023-06-01" -H "content-type: application/json")
  [ -n "$BETA" ] && hdr+=(-H "anthropic-beta: ${BETA}")
  curl -s -o "$2" -w '%{http_code}' -X POST https://api.anthropic.com/v1/messages "${hdr[@]}" --data-binary @"$1"
}

blocks_from_sse() { # rebuild the content blocks of a streamed response
  grep '^data: ' "$1" | sed 's/^data: //' | jq -c -s '
    reduce (.[] | select(.type)) as $e ([];
      if $e.type=="content_block_start" then .[$e.index] = $e.content_block
      elif $e.type=="content_block_delta" then
        (if $e.delta.type=="thinking_delta" then .[$e.index].thinking += $e.delta.thinking
         elif $e.delta.type=="signature_delta" then .[$e.index].signature = ((.[$e.index].signature // "") + $e.delta.signature)
         elif $e.delta.type=="text_delta" then .[$e.index].text += $e.delta.text
         elif $e.delta.type=="input_json_delta" then .[$e.index]._json = ((.[$e.index]._json // "") + $e.delta.partial_json)
         else . end)
      else . end)
    | map(if .type=="tool_use" then (.input = ((._json // "") | if .=="" then {} else fromjson end) | del(._json)) else . end)'
}

summarize_sse() { # event shapes and block summary, no signature values
  echo "  event types: $(grep '^data: ' "$1" | sed 's/^data: //' | jq -r '.type + (if .delta.type then "/" + .delta.type elif .content_block.type then "/" + .content_block.type else "" end)' | sort | uniq -c | tr '\n' ';' | tr -s ' ')"
  echo "  block_start keys: $(grep '^data: ' "$1" | sed 's/^data: //' | jq -c 'select(.type=="content_block_start") | {i:.index, t:.content_block.type, keys:(.content_block|keys)}' | tr '\n' ' ')"
  echo "  signature_delta count per index: $(grep '^data: ' "$1" | sed 's/^data: //' | jq -c 'select(.delta.type=="signature_delta") | .index' | sort | uniq -c | tr '\n' ';' | tr -s ' ')"
  echo "  stop_reason: $(grep '^data: ' "$1" | sed 's/^data: //' | jq -r 'select(.type=="message_delta") | .delta.stop_reason')"
  echo "  error: $( { grep '^data: ' "$1" | sed 's/^data: //'; grep -v '^data: \|^event: \|^$' "$1"; } | jq -r 'select(.type=="error") | .error.message' 2>/dev/null | head -1)"
}

summarize_blocks() { # $1 = blocks json
  echo "$1" | jq -r 'to_entries[] | "  [\(.key)] \(.value.type)" +
    (if .value.type=="thinking" then " thinking_len=\(.value.thinking|length) sig_len=\(.value.signature // "" | length)"
     elif .value.type=="redacted_thinking" then " data_len=\(.value.data // "" | length)"
     elif .value.type=="tool_use" then " id=\(.value.id) name=\(.value.name) input=\(.value.input|tojson)"
     elif .value.type=="text" then " text_len=\(.value.text|length)" else "" end)'
}

result_for() { # tool_result content for a tool_use block
  echo "$1" | jq -c '[.[] | select(.type=="tool_use") | {type:"tool_result", tool_use_id:.id,
    content:(if .name=="lookup_city_code" then "PAR-7351" else "18 C, light rain" end)}]'
}

stream_turn() { # $1=name, $2=messages json, $3=thinking json or "null", $4=model -> writes $OUT/$1.sse
  jq -n --argjson tools "$TOOLS" --argjson msgs "$2" --argjson th "$3" --argjson extra "$EXTRA" --arg m "$4" '
    {model:$m, max_tokens:8000, stream:true, tools:$tools, messages:$msgs} + $extra + (if $th==null then {} else {thinking:$th} end)' > "$OUT/$1.req.json"
  echo "== $1: HTTP $(call "$OUT/$1.req.json" "$OUT/$1.sse" "$4")"
  summarize_sse "$OUT/$1.sse"
}

variant() { # $1=name, $2=messages json, $3=thinking json or "null", $4=model (optional)
  local m="${4:-$MODEL}"
  jq -n --argjson tools "$TOOLS" --argjson msgs "$2" --argjson th "$3" --argjson extra "$EXTRA" --arg m "$m" '
    {model:$m, max_tokens:8000, tools:$tools, messages:$msgs} + $extra + (if $th==null then {} else {thinking:$th} end)' > "$OUT/$1.req.json"
  local code; code="$(call "$OUT/$1.req.json" "$OUT/$1.resp.json" "$m")"
  printf '%-34s HTTP %s  stop=%s  blocks=%s  %s\n' "$1" "$code" \
    "$(jq -r '.stop_reason // "-"' "$OUT/$1.resp.json")" \
    "$(jq -r '[.content[]?.type] | join(",")' "$OUT/$1.resp.json")" \
    "in=$(jq -r '.usage.input_tokens // "-"' "$OUT/$1.resp.json") $(jq -r '.error.message // ""' "$OUT/$1.resp.json" | cut -c1-160)"
}

echo "== model: $MODEL  thinking: $THINKING  beta: ${BETA:-none}  extra: $EXTRA"
U1="$(jq -n --arg p "$PROMPT" '[{role:"user", content:$p}]')"

# 1. Turn 1, streamed.
stream_turn 1-turn1 "$U1" "$THINKING" "$MODEL"
B1="$(blocks_from_sse "$OUT/1-turn1.sse")"; echo "$B1" > "$OUT/1-blocks.json"
summarize_blocks "$B1"
R1="$(result_for "$B1")"
if [ "$(echo "$R1" | jq length)" = 0 ]; then echo "turn 1 made no tool call; stop"; exit 1; fi

STRIP='[.[] | select(.type!="thinking" and .type!="redacted_thinking")]'
M2() { jq -n --argjson u "$U1" --argjson a "$1" --argjson r "$R1" '$u + [{role:"assistant", content:$a}, {role:"user", content:$r}]'; }

echo "== second turns (non-streamed)"
variant 3-real-as-received "$(M2 "$B1")" "$THINKING"
variant 4-no-thinking-blocks-enabled "$(M2 "$(echo "$B1" | jq -c "$STRIP")")" "$THINKING"
variant 5-no-thinking-blocks-disabled "$(M2 "$(echo "$B1" | jq -c "$STRIP")")" null
variant 6-thinking-kept-disabled "$(M2 "$B1")" null
variant 7-tampered-signature "$(M2 "$(echo "$B1" | jq -c 'map(if .type=="thinking" then .signature = (.signature[0:-6] + "AAAAAA") else . end)')")" "$THINKING"
variant 8-tool-use-before-thinking "$(M2 "$(echo "$B1" | jq -c '[.[] | select(.type=="tool_use")] + [.[] | select(.type!="tool_use")]')")" "$THINKING"
variant 9-thinking-text-edited "$(M2 "$(echo "$B1" | jq -c 'map(if .type=="thinking" then .thinking = (.thinking + " ") else . end)')")" "$THINKING"
if [ -n "${MODEL_2:-}" ]; then
  variant "10-other-model-$MODEL_2" "$(M2 "$B1")" "$THINKING" "$MODEL_2"
else
  echo "10-other-model                     skipped (set MODEL_2)"
fi

# 11. Turn 2 streamed, then turn 3 (a longer assistant turn with two tool rounds).
stream_turn 11-turn2 "$(M2 "$B1")" "$THINKING" "$MODEL"
B2="$(blocks_from_sse "$OUT/11-turn2.sse")"; echo "$B2" > "$OUT/11-blocks.json"
summarize_blocks "$B2"
R2="$(result_for "$B2")"
if [ "$(echo "$R2" | jq length)" -gt 0 ]; then
  M3() { jq -n --argjson m2 "$(M2 "$1")" --argjson a "$2" --argjson r "$R2" '$m2 + [{role:"assistant", content:$a}, {role:"user", content:$r}]'; }
  echo "== third turns (non-streamed)"
  variant 12-turn3-real "$(M3 "$B1" "$B2")" "$THINKING"
  variant 13-turn3-first-thinking-stripped "$(M3 "$(echo "$B1" | jq -c "$STRIP")" "$B2")" "$THINKING"
  variant 14-turn3-last-thinking-stripped "$(M3 "$B1" "$(echo "$B2" | jq -c "$STRIP")")" "$THINKING"
  variant 15-turn3-all-stripped-disabled "$(M3 "$(echo "$B1" | jq -c "$STRIP")" "$(echo "$B2" | jq -c "$STRIP")")" null
  # 18. Compaction case: the full history with thinking blocks, to a model with no thinking, no thinking setting.
  if [ -n "${NOTHINK_MODEL:-}" ]; then
    variant "18-nothink-model-$NOTHINK_MODEL" "$(M3 "$B1" "$B2")" null "$NOTHINK_MODEL"
  else
    echo "18-nothink-model                   skipped (set NOTHINK_MODEL)"
  fi
  # 19. A final answer with its thinking kept, then a new user question.
  FINAL="$(jq -c '.content // []' "$OUT/12-turn3-real.resp.json")"
  M4="$(jq -n --argjson m3 "$(M3 "$B1" "$B2")" --argjson a "$FINAL" '$m3 + [{role:"assistant", content:$a}, {role:"user", content:"Thanks. Now do the same for Rome."}]')"
  variant 19-final-answer-kept-then-user "$M4" "$THINKING"
  variant 20-final-answer-stripped-then-user "$(echo "$M4" | jq -c '.[-2].content |= [.[] | select(.type!="thinking" and .type!="redacted_thinking")]')" "$THINKING"
  # 21. A thinking-only assistant message (a hand-off transcript), then a user message.
  THONLY="$(echo "$B1" | jq -c '[.[] | select(.type=="thinking" or .type=="redacted_thinking")]')"
  if [ "$(echo "$THONLY" | jq length)" -gt 0 ]; then
    variant 21-thinking-only-assistant "$(jq -n --argjson u "$U1" --argjson a "$THONLY" '$u + [{role:"assistant", content:$a}, {role:"user", content:"Go on."}]')" "$THINKING"
  else
    echo "21-thinking-only-assistant         skipped (turn 1 had no thinking)"
  fi
else
  echo "turn 2 made no tool call; third turns skipped"
fi

# 16. Redacted thinking, streamed, then the round trip.
UR="$(jq -n --arg p "$REDACT_TRIGGER $BASE_PROMPT" '[{role:"user", content:$p}]')"
stream_turn 16-redacted "$UR" "$THINKING" "$MODEL"
BR="$(blocks_from_sse "$OUT/16-redacted.sse")"; echo "$BR" > "$OUT/16-blocks.json"
summarize_blocks "$BR"
RR="$(echo "$BR" | jq -c '[.[] | select(.type=="tool_use") | {type:"tool_result", tool_use_id:.id, content:"PAR-7351"}]')"
if [ "$(echo "$RR" | jq length)" -gt 0 ]; then
  variant 17-redacted-round-trip "$(jq -n --argjson u "$UR" --argjson a "$BR" --argjson r "$RR" '$u + [{role:"assistant", content:$a}, {role:"user", content:$r}]')" "$THINKING"
else
  echo "17-redacted-round-trip             skipped (no tool call)"
fi
echo "== raw files in $OUT (they hold signatures: do not commit them)"
