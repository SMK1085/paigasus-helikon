# SMA-844 live test results

Date: 2026-11-22. Region us-east-1, profile maip-dev-admin. Command: `cargo test -p paigasus-helikon-providers-bedrock --test live live_typed_memory -- --ignored --nocapture`.

| Model | Test | Result | Note |
|---|---|---|---|
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_two_turns | PASS | Turn 1 calls memory; turn 2 gives text. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_forced_choice | PASS | memory call, finish ToolCalls. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_forced_structured_output | FAIL (before the fix) | 3 of 3 runs. Stream error: `structured-output synthesis: model fired both a real tool and the synthesized output tool '__paigasus_structured_output__'`. Matches spec probe p19 (parallel memory call). |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_native_structured_after_round | PASS | Extra run. Text `{"capital": "Paris"}`. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_none_after_round | PASS | Text answer, no tool call. |
| us.anthropic.claude-sonnet-5-5 | live_typed_memory_two_turns | PASS | Turn 1 calls memory; turn 2 gives text. |
| us.anthropic.claude-sonnet-5-5 | live_typed_memory_forced_choice | PASS | memory call, finish ToolCalls (fallback or direct; not distinguished). |
| us.anthropic.claude-sonnet-5-5 | live_typed_memory_forced_structured_output | PASS | Extra run. Model called memory, no error. |
| us.anthropic.claude-sonnet-5-5 | live_typed_memory_native_structured_after_round | PASS | Text `{"capital":"Paris"}`. |
| us.anthropic.claude-sonnet-5-5 | live_typed_memory_none_after_round | PASS | Text answer, no tool call. |

## After the fix (`disable_parallel_tool_use` on the forced synthesized choice)

| Model | Test | Result | Note |
|---|---|---|---|
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_two_turns | PASS | After the fix. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_forced_choice | PASS | After the fix. memory call, finish ToolCalls. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_forced_structured_output | PASS | After the fix. Text `{"capital": "Paris"}`, finish Stop, no error. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_native_structured_after_round | PASS | After the fix. |
| us.anthropic.claude-sonnet-4-6 | live_typed_memory_none_after_round | PASS | After the fix. |
