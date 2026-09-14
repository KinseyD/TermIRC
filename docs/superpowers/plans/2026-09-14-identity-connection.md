# Identity and Connection Commands Implementation Plan

> Execute in this session using TDD. User approved the seven-command scope and finite automatic reconnection.

**Goal:** Implement `/nick`, `/away`, `/back`, `/connect`, `/disconnect`, `/reconnect`, `/quit` and accurate server/channel status dots.

**Architecture:** Keep pure slash parsing; validate supported commands into typed actions. Each server owns a long-lived worker with a separate control queue, bounded message queue, cancellable connection attempts and bounded retries. Typed events update App identity and per-view connection states. The existing 50 ms UI loop animates grey dots using elapsed time, independent of terminal blink support.

**Tech Stack:** Existing Rust, Tokio, irc, Ratatui and tracing dependencies.

**Spec:** User-approved scope in this conversation. Connection commands target configured server keys (case-insensitive), defaulting to the active server. `/disconnect [reason]` and `/quit [reason]` disconnect the active server; `/connect [server]` connects if stopped; `/reconnect [server]` restarts that connection. `/away [reason]` toggles with a default reason when omitted; `/back` clears away. Identity changes wait for server acknowledgement. No slash input or local command feedback enters chat/status UI; fixed DEBUG log metadata only.

## Constraints

- Branch `feat/identity-connection-commands`; preserve existing untracked assessment/plan documents.
- Green server after 001; green channel after own JOIN. JOIN rejection, own PART/KICK and stopped connections are red. Pending connections/joins/retries blink grey.
- Retry unexpected disconnects/failures three times with 1/2/4 second delays. Connect and registration timeout 30 seconds; manual disconnect/reconnect cancels waits. Authentication/nickname registration failures stop immediately.
- Keep confirmed nickname for reconnect, clear away on a new connection. Drop queued messages on session end; never replay chat after reconnect.
- Preserve channel histories and ordinary console behavior. No new dependencies, no authentication/account-management scope expansion.

## Tasks

- [x] Command validation: literal table tests for supported actions, malformed arguments, unknown commands and CR/LF/NUL rejection; observe failure, implement typed actions.
- [x] Worker lifecycle: localhost mock tests for registration state, JOIN results, acknowledged NICK/AWAY, cancelled registration/backoff, bounded failures, manual disconnect/reconnect, wire QUIT, rejected nickname without disconnection; observe failure, implement supervisor and control messages in `src/irc.rs` and `src/connection.rs`.
- [x] UI wiring: test real submission handler for command routing/no echo/no status feedback and nickname echo; implement routing and typed App state. Buffer tests verify dot symbols/colors and deterministic blinking without shifting labels or cursor.
- [x] Documentation and validation: update README command syntax and retry policy. Run full offline tests, fmt, clippy. Independently review resulting change, fix findings and commit feature files on new branch.

## Execution results

- TDD failures observed before implementation: command actions unavailable, premature connected status, absent sidebar dots, commands not reaching the control queue, missing input/cursor restoration, stale nickname in local echo.
- Additional regressions reproduced and fixed: AWAY acknowledgement leaking into the console; queued old Connected/JOIN events overriding manual disconnect/reconnect. Worker acknowledgement barriers preserve the latest requested state, including consecutive requests.
- Final verification: 200 tests passed (174 library, 7 binary, 7 lifecycle integration, 1 logging integration, 11 existing mock IRC integration); fmt, clippy with warnings denied, and diff whitespace checks passed.
- Independent review found one state-ordering issue; the fix passed follow-up review. No remaining blocking findings.
- Validation used local mock IRC servers and Ratatui buffer rendering; no real configured IRC account was contacted.
