import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, UiCopyArgs } from 'claude-code'

import type { Panel, View } from '../types'

const PANE = 'hookwatch'
const REFRESH_MS = 15_000
// Change the tag when View or Panel changes shape: it declines what an
// earlier version stored, which outlives a reload.
const view = atom({ plugin: 'hookwatch', key: 'view' } as const, { kind: 'empty' } as View, {
  shape: 'view-1',
})

function ago(nowMs: number, tsMs: number): string {
  const s = Math.max(0, Math.round((nowMs - tsMs) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.round(s / 60)}m`
  if (s < 86400) return `${Math.round(s / 3600)}h`
  return `${Math.round(s / 86400)}d`
}

function mb(bytes: number): string {
  return `${(bytes / 1048576).toFixed(1)} MB`
}

function secs(ms: number): string {
  return `${Math.round(ms / 1000)}s`
}

// Read the ledger through the hookwatch command (read-only), for the
// session's repo: `process.run` runs in the session's cwd.
async function refresh($: EngineInterface): Promise<void> {
  const now = Date.now()
  try {
    const r = await $.process.run(['hookwatch', 'panel', '--repo', '.'], { timeoutMs: 10_000 })
    if (r.exitCode !== 0) {
      const why = r.stderr.trim() || `exit ${r.exitCode}`
      const hint = why.includes('unrecognized subcommand') ? ' (run `just install` in the hookwatch repo)' : ''
      await update($, view, (): View => ({ kind: 'error', message: why + hint, read_ms: now }))
      return
    }
    const panel = JSON.parse(r.stdout) as Panel
    await update($, view, (): View => ({ kind: 'ok', panel, read_ms: now }))
  } catch (err) {
    await update($, view, (): View => ({ kind: 'error', message: String(err), read_ms: now }))
  }
}

// The human-only commands (labels, accept, reject) need a terminal, which the
// mod's processes do not have, and that is what keeps an agent from running
// them. So a button only puts the command on the clipboard; the person pastes
// it into a terminal.
async function copyCommand($: EngineInterface, surface: UiCopyArgs['surface'], text: string): Promise<void> {
  const r = await $.ui.copy({ text, surface })
  $.ui.toast(r.isCopied ? `copied: ${text} — paste it in a terminal` : `could not copy (${r.reason}): ${text}`)
}

// Not "hookwatch": the plugin's skill of that name takes /hookwatch:hookwatch,
// and the command did not show beside it (2026-11-14).
const COMMAND = 'hookwatch-pane'

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: COMMAND,
      description: 'Open the hookwatch pane: jigs, proposals, verdicts and the stopwatch of this repo',
    })
    await refresh($)
    void $.ui.open({ id: PANE, title: 'hookwatch' })
    $.clock.every(REFRESH_MS, () => refresh($))

    return next(e)
  })

  on('command.run', { command: COMMAND }, async $ => {
    await refresh($)
    await $.ui.open({ id: PANE, title: 'hookwatch' })

    return { text: 'hookwatch pane opened.' }
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button } = $.ui.resolve(e)
    const v = await read($, view)
    const refreshButton = <Button onPress={() => refresh($)}>Refresh</Button>

    if (v.kind === 'empty') {
      return (
        <Box flexDirection="column">
          <Text dimColor>Reading the ledger…</Text>
          {refreshButton}
        </Box>
      )
    }
    if (v.kind === 'error') {
      return (
        <Box flexDirection="column">
          <Text color="red">hookwatch could not be read</Text>
          <Text>{v.message}</Text>
          {refreshButton}
        </Box>
      )
    }

    const p = v.panel
    const sw = p.stopwatch
    const precision = (right: number, wrong: number) =>
      right + wrong === 0 ? '-' : `${Math.round((100 * right) / (right + wrong))}%`

    return (
      <Box flexDirection="column">
        <Text bold>Status</Text>
        <Text>
          {p.status.events} events · {mb(p.status.file_bytes)} · pruned{' '}
          {p.status.last_prune_ms === null ? 'never' : `${ago(p.now_ms, p.status.last_prune_ms)} ago`}
        </Text>
        {p.status.attention.map(line => (
          <Text color="yellow">! {line}</Text>
        ))}

        <Text bold>Stopwatch (this repo)</Text>
        <Text>
          turns {sw.turns} · cut-ins {sw.cut_ins} · rework {sw.rework_files} files / {sw.rework_lines} lines
        </Text>
        <Text>
          waits: permission {secs(sw.permission_wait_ms)} · question {secs(sw.question_wait_ms)} · denied{' '}
          {sw.denied} · tool failures {sw.tool_failures}
        </Text>

        <Text bold>Jigs</Text>
        {p.jigs.map(j => (
          <Text dimColor={j.lifecycle === 'retired'}>
            {j.lifecycle.padEnd(8)} {j.action.padEnd(6)} {j.id} · fires {j.fires} · precision{' '}
            {precision(j.right, j.wrong)}
          </Text>
        ))}

        <Text bold>Proposals</Text>
        {p.proposals.length === 0 && <Text dimColor>none</Text>}
        {p.proposals.map(x => (
          <Box flexDirection="row" gap={1}>
            <Text color={x.status === 'open' ? 'yellow' : undefined}>
              {x.id} {x.status.padEnd(9)} {x.outlet.padEnd(10)} {x.intent}
            </Text>
            {x.status === 'open' && (
              <Button key={`accept-${x.id}`} onPress={press => copyCommand($, press.surface, `hookwatch accept ${x.id}`)}>
                accept
              </Button>
            )}
            {x.status === 'open' && (
              <Button
                key={`reject-${x.id}`}
                onPress={press => copyCommand($, press.surface, `hookwatch reject ${x.id} --why '...'`)}
              >
                reject
              </Button>
            )}
          </Box>
        ))}

        <Text bold>Recent verdicts</Text>
        {p.verdicts.length === 0 && <Text dimColor>none</Text>}
        {p.verdicts.map(x => (
          <Box flexDirection="row" gap={1}>
            <Text dimColor={!x.applied}>
              {x.id} {ago(p.now_ms, x.ts_ms).padStart(4)} {x.jig} {x.applied ? x.action : `would ${x.action}`}{' '}
              {x.label ?? 'unlabelled'}
            </Text>
            {x.label === null && (
              <Button key={`right-${x.id}`} onPress={press => copyCommand($, press.surface, `hookwatch override ${x.id} --right`)}>
                right
              </Button>
            )}
            {x.label === null && (
              <Button key={`wrong-${x.id}`} onPress={press => copyCommand($, press.surface, `hookwatch override ${x.id} --wrong`)}>
                wrong
              </Button>
            )}
          </Box>
        ))}

        <Text dimColor>
          read {ago(Date.now(), v.read_ms)} ago · a button copies the command; labels, accept and prune run in your
          terminal
        </Text>
        {refreshButton}
      </Box>
    )
  })
}
