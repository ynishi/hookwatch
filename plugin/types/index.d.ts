/** What `hookwatch panel --json` answers (the fields the pane reads). */
export type Jig = {
  id: string
  lifecycle: string
  action: string
  description: string
  fires: number
  applied: number
  wrong: number
  right: number
}

export type Proposal = { id: string; ts_ms: number; status: string; outlet: string; intent: string }

export type Verdict = {
  id: string
  ts_ms: number
  jig: string
  action: string
  applied: boolean
  label: string | null
  evidence: string
}

export type Panel = {
  now_ms: number
  status: {
    events: number
    file_bytes: number
    last_prune_ms: number | null
    open_proposals: number
    unlabelled_applied: number
    attention: string[]
  }
  stopwatch: {
    repo: string | null
    sessions: number
    turns: number
    cut_ins: number
    permission_wait_ms: number
    question_wait_ms: number
    denied: number
    tool_failures: number
    rework_files: number
    rework_lines: number
  }
  jigs: Jig[]
  proposals: Proposal[]
  verdicts: Verdict[]
}

/** The last read of the ledger, or why it could not be read. */
export type View =
  | { kind: 'empty' }
  | { kind: 'ok'; panel: Panel; read_ms: number }
  | { kind: 'error'; message: string; read_ms: number }

declare module 'claude-code' {
  interface PluginState {
    'hookwatch': { view: Shaped<View> }
  }
}
