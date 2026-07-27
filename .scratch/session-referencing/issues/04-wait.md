# 04 gw wait

Status: resolved

Level-triggered bounded wait; results done/attention/error/stale/idle/
ended/timeout; default --timeout 45; 0 = single query; self-wait rejected.
1s poll of the target session record + periodic pid liveness. Core appends
wait_start/wait_end to the waiter's log (ppid ancestor chain). Derivation:
wait events are status-neutral; replay into waiting_on; cleared by matching
wait_end or any later provider event; activity kinds WaitStarted/WaitEnded
mapped in panel.

## Comments

Implemented: `gw wait <addr> [--timeout secs] [--json]`, level-triggered with
1s event-log poll + process-liveness check (pid must still match a provider
process — pid reuse does not count as alive; never-located pid falls back to
events + timeout). Results: done|attention|error|stale|idle|ended|timeout;
default 45s; `--timeout 0` single query; self-wait rejected. Paired
`wait_start`/`wait_end` annotations go to the waiter's log (ppid ancestor
chain); unidentifiable waiter records nothing; wait_end written even on
timeout/error. Replay: status-neutral, `waiting_on` derivation, leftover
open waits cleared by the waiter's next provider event but not by focus
events — covered by session.rs tests (`wait_events_are_status_neutral`,
`waiting_on_replays_pairs_and_provider_events_clear_leftovers`,
`wait_events_appear_in_activity`). TUI shows `wait+`/`wait-` activity;
operational events never notify.
