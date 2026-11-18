// RimZ's Claude Code mod. `rimz hooks install` writes this folder and lists it
// in the account's settings `env.CLAUDE_CODE_PLUGIN_DIRS`. It only observes:
// every hook awaits `next(e)` first and returns exactly what it resolved, and a
// report that fails, throws or outlasts its time is dropped, so the mod never
// changes or breaks a session. RimZ's settings hooks stay the lifecycle truth.
import type { Register } from 'claude-code'

// Bumped by hand when this folder's files change; `rimz` reads it from each
// report, and an install rewrites only files whose content differs.
const MOD_VERSION = '1.1.0'
const REPORT_TIMEOUT_MS = 2000

// One envelope on the stdin of `rimz mod feed --source claude`, argv and no
// shell. Every field but `v`, `event` and `session_id` is optional to the feed.
// `usage` is `$.session.usage()` as Claude answered it, read at each report so
// the readings move with every model request; `reason` is `turn.complete`'s.
const report = async ($: any, event: string, agentId?: string, reason?: string): Promise<void> => {
  try {
    const info = await $.session.info().catch(() => undefined)
    const version = await $.session.version().catch(() => undefined)
    const agents = await Promise.resolve()
      .then(() => $.agent.list())
      .catch(() => undefined)
    const usage = await Promise.resolve()
      .then(() => $.session.usage())
      .catch(() => undefined)
    const envelope = {
      v: MOD_VERSION,
      event,
      session_id: await $.session.id(),
      transcript_path: info?.transcriptPath,
      claude_version: version?.version,
      agent_id: agentId,
      agents,
      usage,
      reason,
    }
    const rimz = (await $.env.get('RIMZ_BIN')) || 'rimz'
    await $.process.run([rimz, 'mod', 'feed', '--source', 'claude'], {
      stdin: JSON.stringify(envelope),
      timeoutMs: REPORT_TIMEOUT_MS,
    })
  } catch {
    // Reporting is best effort: the session never sees a failed report.
  }
}

// The child id a loop event carries; main-loop events carry none.
const childOf = (e: any): string | undefined =>
  typeof e?.agentId === 'string' && e.agentId !== '' ? e.agentId : undefined

// Each hook reports once `next(e)` resolved, so no report runs while a
// permission prompt is open: at session start (the check-in), once a child is
// spawned, after every tool call, and at the end of every turn. `rimz` maps
// `$.agent.list()` onto its rows and scans the transcript the report names.
export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const result = await next(e)
    await report($, 'session.start')
    return result
  })
  on('agent.spawn', async ($, e, next) => {
    const result = await next(e)
    await report($, 'agent.spawn', childOf(result))
    return result
  })
  on('tool.call', async ($, e, next) => {
    const result = await next(e)
    await report($, 'tool.call', childOf(e))
    return result
  })
  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    const reason = typeof e?.reason === 'string' ? e.reason : undefined
    await report($, 'turn.complete', childOf(e), reason)
    return result
  })
}
