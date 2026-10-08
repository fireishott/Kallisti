// dsh-phone-api — native HTTP+SSE surface for the Kallisti iOS client.
// Runs in the DSH host process with a real AbortController, and lives in the
// host composition so it survives restarts.

import { readFile, writeFile, rename, mkdir, readdir, unlink } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { createRequire } from 'node:module'
import { spawn, execFile } from 'node:child_process'
import { createMediaHandler } from './media.js'

// `yaml` is not installed beside this plugin. Resolve it through the running
// DSH entry point so the parser is the exact one DSH's config editor uses,
// with no separate install to drift. Falls back to parse-free validation.
let yamlLib = null
try {
  const req = createRequire(process.argv[1] || import.meta.url)
  yamlLib = req(req.resolve('yaml'))
} catch (e) { yamlLib = null }

const name = 'dsh-phone-api'
const inject = ['webServer', 'sessionController', 'sessionSkillCatalog', 'fs', 'attachments']

// Shared secret the app sends as a bearer token. Set it in the environment DSH
// runs under; with no token every request is rejected.
const TOKEN = process.env.KALLISTI_DSH_TOKEN || ''

// DSH config editing. The editable document is the web profile's patch layer,
// the same file DSH's own Settings writes (dsh-config-editor). The profile's
// HMR watcher reconciles it live on write, so no restart is needed.
const PATCH_PATH = join(homedir(), '.dsh', 'profiles', 'web', 'cordis.patch.yml')
const BACKUP_DIR = join(homedir(), '.dsh', 'profiles', 'web', '.config-backups')
const MAX_CONFIG_BYTES = 1024 * 1024

// Parse check only: the file is a YAML sequence of loader patch entries and
// may carry `!!js` expressions, which must survive untouched.
function validatePatchYaml(text) {
  if (!yamlLib) return null
  const { parseDocument, isSeq, isMap } = yamlLib
  const doc = parseDocument(text, {
    customTags: [{ tag: 'tag:yaml.org,2002:js', resolve: (v) => v }],
  })
  if (doc.errors.length > 0) {
    const e = doc.errors[0]
    const line = e.linePos && e.linePos[0] ? ' (line ' + e.linePos[0].line + ')' : ''
    return e.message.split('\n')[0] + line
  }
  if (!isSeq(doc.contents)) return 'Profile patch must be a YAML sequence (a top-level list)'
  for (const item of doc.contents.items) {
    if (!isMap(item)) return 'Every patch entry must be a mapping'
    if (!item.has('id') && !item.has('insert')) return 'Every patch entry needs an id or an insert list'
  }
  return null
}

function readBody(req) {
  return new Promise((resolve) => {
    let acc = ''
    req.setEncoding('utf8')
    req.on('data', (c) => { acc += c })
    req.on('end', () => resolve(acc))
    req.on('error', () => resolve(''))
  })
}

function sendJson(res, code, obj) {
  const body = JSON.stringify(obj)
  res.writeHead(code, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body),
  })
  res.end(body)
}

function authed(req) {
  return TOKEN !== '' && req.headers && req.headers['authorization'] === 'Bearer ' + TOKEN
}

function query(req) {
  const out = {}
  const i = req.url.indexOf('?')
  if (i < 0) return out
  for (const pair of req.url.slice(i + 1).split('&')) {
    const eq = pair.indexOf('=')
    if (eq < 0) continue
    out[decodeURIComponent(pair.slice(0, eq))] = decodeURIComponent(pair.slice(eq + 1))
  }
  return out
}

const fail = (res, code, message) => sendJson(res, code, { error: message })
const startedAt = Date.now()

// ---------------------------------------------------------------------------
// Prompt idempotency + job status.
//
// The app mints one clientMessageId per user message and reuses it on every
// resend. It becomes the DSH prompt requestId (`kallisti-<id>`), which DSH
// persists as the user message's `source.rpcId` and already dedupes within a
// Session. The ledger below is the cross-Session backstop: a resend that lands
// on a different Session (lost local mapping) is answered from the original
// Session instead of starting a second turn. It also lets GET /job resolve a
// clientMessageId without the caller knowing the Session.
// ---------------------------------------------------------------------------
const LEDGER_PATH = join(homedir(), '.dsh', 'plugins', 'dsh-phone-api', 'state', 'prompts.json')
const LEDGER_MAX = 2000
let ledger = null // Map<clientMessageId, { sessionId, requestId, at }>

async function loadLedger() {
  if (ledger) return ledger
  ledger = new Map()
  try {
    const rows = JSON.parse(await readFile(LEDGER_PATH, 'utf8'))
    for (const r of rows) if (r && r.id && r.sessionId) ledger.set(r.id, r)
  } catch (e) { /* first run or unreadable: start empty */ }
  return ledger
}

let ledgerWrite = Promise.resolve()
function saveLedger() {
  const rows = [...ledger.values()].sort((a, b) => a.at - b.at).slice(-LEDGER_MAX)
  ledgerWrite = ledgerWrite.then(async () => {
    try {
      await mkdir(join(LEDGER_PATH, '..'), { recursive: true, mode: 0o700 })
      const tmp = LEDGER_PATH + '.tmp'
      await writeFile(tmp, JSON.stringify(rows), { mode: 0o600 })
      await rename(tmp, LEDGER_PATH)
    } catch (e) { /* ledger is best effort; DSH's own rpcId dedupe still holds */ }
  })
  return ledgerWrite
}

const normalizeClientId = (v) => (typeof v === 'string' && /^[0-9a-fA-F-]{36}$/.test(v) ? v.toLowerCase() : null)
const requestIdFor = (clientId) => 'kallisti-' + clientId

function textOf(content) {
  if (!Array.isArray(content)) return ''
  let out = ''
  for (const b of content) if (b && b.type === 'text' && typeof b.text === 'string') out += b.text
  return out
}

/**
 * Derive one prompt's job state from the Session's durable events.
 * Pure: exported for the offline test harness.
 *   queued      admitted to the inbox, no turn has consumed it yet
 *   running     its turn started and has not ended
 *   completed   its turn ended normally (text = last model answer after it)
 *   failed      its turn ended in error
 *   cancelled   its turn was aborted (user Stop)
 *   interrupted its turn was cut off (host restart / crash) - never auto-resend
 */
function deriveJob(events, requestId, sessionRunning) {
  let userSeq = -1
  let queued = false
  for (const e of events) {
    const d = e.data || {}
    if (e.type === 'user/message' && d.source && d.source.rpcId === requestId) { userSeq = e.seq; break }
    if (e.type === 'agent/inbox/spliced' && Array.isArray(d.inserted) &&
        d.inserted.some((m) => m && m.source && m.source.rpcId === requestId)) queued = true
  }
  if (userSeq < 0) return queued ? { status: sessionRunning ? 'queued' : 'interrupted' } : null

  let turn = null
  for (const e of events) {
    if (e.seq >= userSeq) break
    if (e.type === 'turn/start' && e.data && typeof e.data.turn === 'number') turn = e.data.turn
  }
  let text = null
  let messageId = null
  let usage = null
  for (const e of events) {
    if (e.seq <= userSeq) continue
    const d = e.data || {}
    if (e.type === 'assistant/message') {
      const m = d.message || {}
      const t = textOf(m.content)
      if (t && (!m.source || m.source.kind === 'model')) { text = t; messageId = m.id || null }
      if (d.usage) usage = d.usage
    }
    if (e.type === 'turn/end' && (turn === null || d.turn === turn)) {
      const kind = (d.reason && d.reason.kind) || 'completed'
      const base = { turn, text, messageId, usage, endedAt: e.time || null }
      if (kind === 'completed') return { status: 'completed', ...base }
      if (kind === 'error') {
        const err = (d.reason && d.reason.error) || {}
        return { status: 'failed', ...base, error: err.message || 'DSH turn failed', errorCode: err.code || null }
      }
      if (kind === 'aborted') return { status: 'cancelled', ...base }
      return { status: 'interrupted', ...base, error: 'The host restarted mid-turn (' + kind + ').' }
    }
  }
  // No turn/end yet. A live turn is running; a dead one was cut off by a
  // hard kill that never got to write turn/end.
  if (sessionRunning) return { status: 'running', turn, text }
  return { status: 'interrupted', turn, text, error: 'The host stopped mid-turn.' }
}

async function jobFor(sc, sessionId, clientId, requestId = requestIdFor(clientId)) {
  let inspection
  try { inspection = await sc.inspect(sessionId) } catch (e) { return null }
  if (!inspection) return null
  let running = false
  try {
    const listed = await sc.list({})
    const row = ((listed && listed.items) || []).find((s) => s.sessionId === sessionId)
    running = !!(row && row.running)
  } catch (e) { /* treat as idle */ }
  const job = deriveJob(inspection.events || [], requestId, running)
  return job ? { ...job, sessionId, requestId, clientMessageId: clientId } : null
}

// Restart the LaunchAgent that owns this process. Must run detached: the
// kickstart kills us, and a child in our process group would die with it.
const LAUNCH_LABEL = 'ai.deepseek.dsh.web'
function launchTarget() { return 'gui/' + process.getuid() + '/' + LAUNCH_LABEL }
function launchAgentLoaded() {
  return new Promise((resolve) => {
    execFile('/bin/launchctl', ['print', launchTarget()], (err) => resolve(!err))
  })
}

// Pending user-question state, keyed by sessionId. Each entry carries the
// questions the model asked plus the resolver that will feed the answer back
// into the running tool call.
const pendingQuestions = new Map()
const QUESTION_TIMEOUT_MS = 10 * 60 * 1000 // 10 minutes, matching clarify_timeout

function sessionIdFromRequest(request) {
  // Scoped user-questions/request carries the live agent; its session id is
  // the durable session identity.
  const agent = request && request.agent
  if (agent && agent.session && agent.session.id) return agent.session.id
  return null
}

function cleanupSession(sessionId) {
  const entry = pendingQuestions.get(sessionId)
  if (entry) {
    if (entry.timeout) clearTimeout(entry.timeout)
    pendingQuestions.delete(sessionId)
  }
}

function apply(ctx) {
  const server = ctx.webServer
  const sc = ctx.sessionController
  const disposers = []
  // Registered before any route: if a later register throws, cordis still
  // runs this disposer and no half-mounted route set outlives the plugin.
  ctx.effect(() => () => {
    for (const d of disposers) { try { d() } catch (e) { /* ignore */ } }
  })

  // Listen for ask_user_question waterfall requests so the phone client can
  // answer them. Web UI listeners run in the agent scope and normally win;
  // when the user is on Kallisti (no Web UI scoped listener), this host-level
  // listener receives the request and parks it for the phone API.
  //
  // PREPENDED and RACED: the web-remote forwarder (dsh-api-remotes) is an
  // earlier listener that never calls next() while a browser tab is attached,
  // and with no tab the gateway parks the event with no deliveries. A plain
  // listener therefore never saw a question, and every phone answer 404'd.
  // This listener runs first, parks the question for the phone, and forwards
  // to the rest of the chain concurrently. First answer wins; a phone answer
  // aborts the forwarded copy so the web GUI card is withdrawn.
  const disposeQuestionListener = ctx.on('user-questions/request', (request, next) => {
    const sessionId = sessionIdFromRequest(request)
    if (!sessionId) {
      // Unscoped request: nothing to map to a phone session; delegate.
      return next()
    }
    // If a question is already parked for this session, replace it. Only the
    // most recent unanswered question batch is answerable at a time.
    const existing = pendingQuestions.get(sessionId)
    if (existing) {
      if (existing.timeout) clearTimeout(existing.timeout)
      existing.reject(new Error('superseded by newer question'))
    }
    const phoneAbort = new AbortController()
    const original = request.signal
    // `next()` re-dispatches this same request object, so widening its signal
    // is how a phone answer cancels the forwarded web copy.
    try {
      request.signal = original ? AbortSignal.any([original, phoneAbort.signal]) : phoneAbort.signal
    } catch (e) { /* frozen request: web copy just lingers until the turn ends */ }

    return new Promise((resolve, reject) => {
      let settled = false
      const finish = (fn, value) => {
        if (settled) return
        settled = true
        const entry = pendingQuestions.get(sessionId)
        if (entry && entry.token === token) cleanupSession(sessionId)
        fn(value)
      }
      const token = {}
      const timeout = setTimeout(() => {
        finish(reject, new Error('user question timed out unanswered'))
      }, QUESTION_TIMEOUT_MS)
      pendingQuestions.set(sessionId, {
        token,
        request,
        timeout,
        resolve: (value) => { finish(resolve, value); phoneAbort.abort(new Error('answered from phone')) },
        reject: (err) => finish(reject, err),
      })
      if (original) {
        original.addEventListener('abort', () => finish(reject, original.reason), { once: true })
      }
      Promise.resolve()
        .then(next)
        .then(
          (value) => finish(resolve, value),
          (err) => {
            // Our own abort, or no web answerer at all: the phone path owns it.
            if (phoneAbort.signal.aborted) return
            if (original && original.aborted) return finish(reject, err)
            if (err && err.code === 'NO_PROVIDER') return
            finish(reject, err)
          },
        )
    })
  }, { prepend: true })
  disposers.push(disposeQuestionListener)

  const route = (path, handler) =>
    disposers.push(server.register({ kind: 'exact', path, handler }))

  // Kallisti turns assistant MEDIA directives into MessageAttachments and
  // fetches this endpoint with its existing DSH bearer token.
  const mediaHandler = createMediaHandler({ secret: TOKEN, token: TOKEN })
  route('/phone/v1/media', (req, res) => mediaHandler(req, res))

  route('/phone/v1/health', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    sendJson(res, 200, {
      ok: true, backend: 'dsh', service: name, version: '1.1.0',
      startedAt, pid: process.pid, capabilities: ['job-status', 'idempotent-prompt', 'restart'],
    })
  })

  route('/phone/v1/models', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    try {
      const cat = await sc.modelCatalog()
      sendJson(res, 200, {
        defaultProvider: cat.default && cat.default.provider,
        defaultModel: cat.default && cat.default.model,
        groups: (cat.groups || []).map((g) => ({
          id: g.id,
          name: g.name,
          models: (g.models || []).map((m) => ({ id: m.id, name: m.name })),
        })),
      })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // Per-session model selection. Takes effect on the session's next model
  // request, the same contract the Web GUI model picker uses.
  route('/phone/v1/model', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      if (!body.sessionId || !body.provider || !body.model) {
        return fail(res, 400, 'sessionId, provider, model required')
      }
      const sel = { sessionId: body.sessionId, provider: body.provider, model: body.model }
      if (body.reasoningEffort) sel.reasoningEffort = body.reasoningEffort
      const v = await sc.selectModel(sel)
      sendJson(res, 200, { selected: v && v.selected })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // GET -> { path, content, size }; PUT { content } -> { ok, path, backup }.
  route('/phone/v1/config', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    try {
      if (req.method === 'GET') {
        const content = await readFile(PATCH_PATH, 'utf8')
        return sendJson(res, 200, {
          path: '~/.dsh/profiles/web/cordis.patch.yml',
          size: Buffer.byteLength(content),
          content,
        })
      }
      if (req.method !== 'PUT') return fail(res, 405, 'GET or PUT required')
      const raw = await readBody(req)
      if (Buffer.byteLength(raw) > MAX_CONFIG_BYTES) return fail(res, 413, 'config too large')
      const body = JSON.parse(raw || '{}')
      if (typeof body.content !== 'string') return fail(res, 400, 'content required')
      const problem = validatePatchYaml(body.content)
      if (problem) return fail(res, 422, problem)
      // Back up the live file, keep the newest 20, then write atomically.
      await mkdir(BACKUP_DIR, { recursive: true, mode: 0o700 })
      const stamp = new Date().toISOString().replace(/[:.]/g, '-')
      const backup = join(BACKUP_DIR, 'cordis.patch.' + stamp + '.yml')
      const before = await readFile(PATCH_PATH, 'utf8')
      await writeFile(backup, before, { mode: 0o600 })
      const old = (await readdir(BACKUP_DIR)).filter((f) => f.endsWith('.yml')).sort()
      for (const f of old.slice(0, Math.max(0, old.length - 20))) {
        try { await unlink(join(BACKUP_DIR, f)) } catch (e) { /* ignore */ }
      }
      const tmp = PATCH_PATH + '.phone-tmp'
      await writeFile(tmp, body.content, { mode: 0o600 })
      await rename(tmp, PATCH_PATH)
      sendJson(res, 200, {
        ok: true,
        path: '~/.dsh/profiles/web/cordis.patch.yml',
        backup: backup.replace(homedir(), '~'),
      })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/config/validate', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      if (typeof body.content !== 'string') return fail(res, 400, 'content required')
      const problem = validatePatchYaml(body.content)
      if (problem) return fail(res, 422, problem)
      sendJson(res, 200, { valid: true })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/sessions', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    try {
      const v = await sc.list({})
      sendJson(res, 200, {
        items: (v.items || []).map((s) => ({
          sessionId: s.sessionId,
          updatedAt: s.updatedAt,
          running: s.running,
          blank: s.blank,
          cwd: s.cwd || null,
          projections: s.projections || null,
        })),
      })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/session', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      const reqObj = { agentPreset: body.agentPreset || 'ignyte' }
      if (body.sessionId) reqObj.sessionId = body.sessionId
      if (body.cwd) reqObj.cwd = body.cwd
      const v = await sc.create(reqObj)
      sendJson(res, 200, { sessionId: v.sessionId, agentPreset: v.agentPreset || null })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/prompt', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      const sid = body.sessionId
      if (!sid) return fail(res, 400, 'sessionId required')
      const text = String(body.text || '')
      const images = Array.isArray(body.images) ? body.images : []
      if (!text.trim() && images.length === 0) return fail(res, 400, 'text or image required')
      if (images.length > 20) return fail(res, 400, 'at most 20 images are allowed')
      const content = text.trim() ? [{ type: 'text', text }] : []
      for (const image of images) {
        if (!image || typeof image.data !== 'string' || typeof image.mimeType !== 'string') {
          return fail(res, 400, 'each image requires base64 data and mimeType')
        }
        if (!['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(image.mimeType)) {
          return fail(res, 400, 'unsupported image mimeType')
        }
        let raw
        try { raw = Buffer.from(image.data, 'base64') } catch (_) { return fail(res, 400, 'invalid image base64') }
        if (raw.length === 0 || raw.toString('base64') !== image.data || raw.length > 20 * 1024 * 1024) {
          return fail(res, 400, 'invalid or oversized image')
        }
        content.push({ type: 'image', data: image.data, mediaType: image.mimeType })
      }
      // Idempotency: a resend of the same clientMessageId never starts a
      // second turn. Same Session -> DSH's own rpcId dedupe answers it.
      // Different Session (the app lost its mapping) -> the ledger does.
      //
      // A prior job that is queued, running, or completed is returned as
      // `duplicate` for the app to attach to. Only a prior job that died
      // (interrupted / failed / cancelled) gets a fresh attempt: that resend
      // is a deliberate RETRY, because the app no longer auto-resends those.
      const clientId = normalizeClientId(body.clientMessageId)
      let reqId = 'phone-' + Date.now() + '-' + Math.random().toString(36).slice(2, 10)
      if (clientId) {
        const book = await loadLedger()
        const prior = book.get(clientId)
        let attempt = 1
        if (prior) {
          const job = await jobFor(sc, prior.sessionId, clientId, prior.requestId).catch(() => null)
          if (job && (job.status === 'queued' || job.status === 'running' || job.status === 'completed')) {
            return sendJson(res, 200, {
              accepted: true, duplicate: true, requestId: prior.requestId,
              sessionId: prior.sessionId, job,
            })
          }
          attempt = (prior.attempt || 1) + (job ? 1 : 0)
        }
        reqId = requestIdFor(clientId) + (attempt > 1 ? '.' + attempt : '')
        book.set(clientId, { id: clientId, sessionId: sid, requestId: reqId, attempt, at: Date.now() })
        await saveLedger()
      }
      await sc.prompt(
        {
          requestId: reqId,
          sessionId: sid,
          mode: body.mode === 'steer' ? 'steer' : 'queue',
          content,
        },
        new AbortController().signal,
      )
      sendJson(res, 200, { accepted: true, requestId: reqId, sessionId: sid })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // Authoritative state of one prompt, keyed by the app's clientMessageId.
  // GET /phone/v1/job?clientMessageId=<uuid>[&sessionId=<id>]
  route('/phone/v1/job', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    const clientId = normalizeClientId(q.clientMessageId)
    if (!clientId) return fail(res, 400, 'clientMessageId (uuid) required')
    try {
      const book = await loadLedger()
      const entry = book.get(clientId) || {}
      const sid = entry.sessionId || q.sessionId
      if (!sid) return fail(res, 404, 'unknown job')
      const job = await jobFor(sc, sid, clientId, entry.requestId)
      if (!job) return fail(res, 404, 'unknown job')
      sendJson(res, 200, job)
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // Restart DSH through its LaunchAgent. POST /phone/v1/restart
  // Replies first, then kickstarts from a detached child so the restart
  // outlives this process. launchd brings DSH back on the same port.
  route('/phone/v1/restart', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    if (!(await launchAgentLoaded())) {
      return fail(res, 409, 'LaunchAgent ' + LAUNCH_LABEL + ' is not loaded; restart DSH from the Mac')
    }
    let running = 0
    try {
      const listed = await sc.list({})
      running = ((listed && listed.items) || []).filter((s) => s.running).length
    } catch (e) { /* informational only */ }
    sendJson(res, 202, { restarting: true, label: LAUNCH_LABEL, interruptedSessions: running, startedAt: startedAt })
    setTimeout(() => {
      const child = spawn('/bin/sh', ['-c', 'sleep 1; /bin/launchctl kickstart -k ' + launchTarget()], {
        detached: true, stdio: 'ignore',
      })
      child.unref()
    }, 250)
  })

  route('/phone/v1/follow', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    const sid = q.sessionId
    if (!sid) return fail(res, 400, 'sessionId required')
    // Headers are withheld until the opening snapshot is in hand. The app
    // treats "headers received" as "snapshot taken" and admits its prompt
    // right after; sending headers first let a fast turn land INSIDE the
    // snapshot, where the client skips it as history and waits forever.
    let opened = false
    const open = () => {
      if (opened) return
      opened = true
      res.writeHead(200, {
        'content-type': 'text/event-stream; charset=utf-8',
        'cache-control': 'no-cache',
        connection: 'keep-alive',
        'x-accel-buffering': 'no',
      })
      res.write(': open\n\n')
    }
    const controller = new AbortController()
    let closed = false
    res.on('close', () => { closed = true; controller.abort() })
    try {
      const stream = sc.follow(
        {
          address: { kind: 'session', sessionId: sid },
          maxMessages: 200,
          assistantStream: true,
        },
        controller.signal,
      )
      for await (const frame of stream) {
        if (closed) break
        open()
        res.write('data: ' + JSON.stringify(frame) + '\n\n')
      }
    } catch (e) {
      if (!closed) {
        if (!opened) return fail(res, 500, String((e && e.message) || e))
        res.write('event: error\ndata: ' + JSON.stringify({ message: String((e && e.message) || e) }) + '\n\n')
      }
    }
    if (!closed) { open(); res.end() }
  })

  route('/phone/v1/cancel', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      if (!body.sessionId) return fail(res, 400, 'sessionId required')
      const v = sc.cancel({ sessionId: body.sessionId })
      sendJson(res, 200, { accepted: !!(v && v.accepted) })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // User-questions support for ask_user_question tool calls.
  // Kallisti polls this endpoint after seeing an ask_user_question tool call
  // and renders the returned question(s) as a ClarifyCard.
  route('/phone/v1/questions', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'GET') return fail(res, 405, 'GET required')
    const q = query(req)
    const sessionId = q.sessionId
    if (!sessionId) return fail(res, 400, 'sessionId required')
    const entry = pendingQuestions.get(sessionId)
    if (!entry) return sendJson(res, 200, { questions: [] })
    sendJson(res, 200, { questions: entry.request.questions || [] })
  })

  // Submit an answer to a pending ask_user_question. The body matches the
  // tool's output schema: { answers: [{ id, selected: [...], custom? }] }.
  route('/phone/v1/answer', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    if (req.method !== 'POST') return fail(res, 405, 'POST required')
    try {
      const body = JSON.parse((await readBody(req)) || '{}')
      const sessionId = body.sessionId
      if (!sessionId) return fail(res, 400, 'sessionId required')
      const entry = pendingQuestions.get(sessionId)
      if (!entry) return fail(res, 404, 'no pending question for session')
      if (!Array.isArray(body.answers)) return fail(res, 400, 'answers array required')
      // Validate that every answered question id exists in the request.
      const ids = new Set((entry.request.questions || []).map((q) => q.id))
      for (const a of body.answers) {
        if (!a || typeof a.id !== 'string') return fail(res, 400, 'each answer requires id')
        if (!ids.has(a.id)) return fail(res, 400, 'unknown question id: ' + a.id)
        if (!Array.isArray(a.selected)) return fail(res, 400, 'each answer requires selected array')
      }
      entry.resolve({ answers: body.answers })
      sendJson(res, 200, { accepted: true })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // Skill catalog + detail.
  //
  // `ctx.skills` is LAYERED PER SCOPE: a preset's skills live in that preset's
  // layer, so a host-plane read of ctx.skills.list() returns nothing. The
  // Session-scoped catalog service resolves the view for a Session's preset,
  // which is why it takes a sessionId.
  route('/phone/v1/skills', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    const signal = new AbortController().signal
    try {
      let sid = q.sessionId
      if (!sid) {
        const listed = await sc.list({})
        const items = (listed && listed.items) || []
        if (items.length === 0) return sendJson(res, 200, { skills: [] })
        sid = items[0].sessionId
      }
      const value = await ctx.sessionSkillCatalog.list({ sessionId: sid }, signal)
      sendJson(res, 200, {
        skills: (value.skills || []).map((sk) => ({
          name: sk.name,
          description: sk.description || '',
          path: '',
        })),
      })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  // Skill bodies live on disk at a known root; the catalog service exposes
  // metadata only, so the body is read through the fs service.
  route('/phone/v1/skill', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    if (!q.name) return fail(res, 400, 'name required')
    const signal = new AbortController().signal
    try {
      // lstat does not expand `~`; resolve the path to an FsTarget first and
      // stat that, otherwise every lookup reports "skill not found".
      const target = await ctx.fs.resolve('~/.dsh/skills/' + q.name + '/SKILL.md', { signal })
      const info = await ctx.fs.stat(target, signal)
      if (!info) return fail(res, 404, 'skill not found')
      const text = await ctx.fs.readText(target, signal)
      let description = ''
      const m = /^---[\s\S]*?description:\s*(.+)$/m.exec(text)
      if (m) description = m[1].trim().replace(/^["']|["']$/g, '')
      sendJson(res, 200, {
        name: q.name,
        description,
        path: '~/.dsh/skills/' + q.name + '/SKILL.md',
        content: text,
      })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/page', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    if (!q.sessionId) return fail(res, 400, 'sessionId required')
    const signal = new AbortController().signal
    try {
      // DSH rejects a throughSeq past the session's committed cursor
      // ("through seq N is past cursor M"), so resolve the real cursor when the
      // caller did not supply one instead of sending Number.MAX_SAFE_INTEGER.
      let throughSeq = q.throughSeq ? Number(q.throughSeq) : undefined
      if (throughSeq === undefined) {
        const inspection = await sc.inspect(q.sessionId, signal)
        const events = (inspection && inspection.events) || []
        if (events.length === 0) {
          return sendJson(res, 200, { hasMore: false, records: [] })
        }
        throughSeq = events[events.length - 1].seq
      }
      const page = await sc.page(
        {
          address: { kind: 'session', sessionId: q.sessionId },
          throughSeq,
          maxMessages: q.maxMessages ? Number(q.maxMessages) : 50,
        },
        signal,
      )
      sendJson(res, 200, { hasMore: page.hasMore, records: page.records })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

}

export { apply, inject, name, deriveJob }
