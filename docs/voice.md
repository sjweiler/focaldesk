# Ambient Voice

The AI Console's **Ambient Voice** workspace is an opt-in interface to the same
permissioned chat, agent, workflow, and capability layers used by typed input.
It does not listen at application startup. Press **Start listening** for the
current Console session and say the default wake phrase, “hello focaldesk.”

`focald-mic` is the system session orchestrator. It authenticates the calling
executable, grants one exclusive microphone lease, and exposes capture state to
the existing shell microphone indicator. Ambient clients renew their lease by
polling; abandoned leases stop capture after 15 seconds. Console chat and agent
dictation use this same owner and cannot open a parallel recorder.

Recognition is local through Vosk. `FOCALDESK_VOSK_MODEL_DIR` can point to a
model, or the Console looks under
`$XDG_DATA_HOME/focaldesk/voice/vosk-model-small-en-us-0.15`. The workspace
shows whether the microphone is off, listening, hearing speech, has detected
the wake phrase, or is routing a command.

## Commands

- `hello focaldesk run workflow morning-briefing` starts that registered
  workflow.
- `hello focaldesk ask agent accessibility to describe the desktop` starts
  that registered agent.
- Other wake-gated text enters the current chat.

Agent and workflow starts use the normal AI IPC path. They retain provider,
tool, resource, budget, capability-lease, permission, and one-shot native
mutation-confirmation checks. Voice input cannot approve a proposed mutation.

Speaking while a chat response is streaming requests cancellation and
immediately interrupts `focald-speech`, providing barge-in across response text
and audio. **Speak responses locally** sends ambient chat responses and agent or
workflow acknowledgements to the local speech daemon. Desktop Agent's **Read
proposal aloud** control narrates the displayed proposal, but it does not press
Approve or alter the native confirmation state.

## Privacy and controls

- Audio callback pressure is bounded; excess chunks are dropped instead of
  accumulating in memory.
- The rolling raw-audio buffer is RAM-only, capped by configuration (three
  seconds by default), and cleared when listening stops, errors, or is killed.
- Transcript retention is disabled by default. When enabled, finalized commands
  are kept only in the page's in-memory text buffer and can be cleared. This
  workspace never writes that transcript to disk.
- **Kill microphone now** closes the daemon-owned session and blocks all
  microphone starts through `focald-mic`, including dictation. **Re-enable
  microphone** only reopens the gate; it does not start capture.
- The blocked-application list is checked before capture. Adding
  `focaldesk-ai-console` denies this workspace access.

The kill gate covers FocalDesk's microphone clients and survives Console
closure because it lives in `focald-mic`. It is not a hardware mute for
unrelated applications; use the desktop's system microphone control when the
device itself must be globally muted.
