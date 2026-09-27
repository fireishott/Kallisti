// dsh-phone-api — native HTTP+SSE surface for the Kallisti iOS client.
// Runs in the DSH host process with a real AbortController, and lives in the
// host composition so it survives restarts.

import { readFile, writeFile, rename, mkdir, readdir, unlink } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { createRequire } from 'node:module'

// `yaml` is not installed beside this plugin. Resolve it through the running
// DSH entry point so the parser is the exact one DSH's config editor uses,
// with no separate install to drift. Falls back to parse-free validation.
let yamlLib = null
try {
  const req = createRequire(process.argv[1] || import.meta.url)
  yamlLib = req(req.resolve('yaml'))
} catch (e) { yamlLib = null }

const name = 'dsh-phone-api'
const inject = ['webServer', 'sessionController', 'sessionSkillCatalog', 'fs']

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

function apply(ctx) {
  const server = ctx.webServer
  const sc = ctx.sessionController
  const disposers = []
  // Registered before any route: if a later register throws, cordis still
  // runs this disposer and no half-mounted route set outlives the plugin.
  ctx.effect(() => () => {
    for (const d of disposers) { try { d() } catch (e) { /* ignore */ } }
  })

  const route = (path, handler) =>
    disposers.push(server.register({ kind: 'exact', path, handler }))

  route('/phone/v1/health', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    sendJson(res, 200, { ok: true, backend: 'dsh', service: name, version: '1.0.0' })
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
      if (!text.trim()) return fail(res, 400, 'text required')
      const reqId = 'phone-' + Date.now() + '-' + Math.random().toString(36).slice(2, 10)
      await sc.prompt(
        {
          requestId: reqId,
          sessionId: sid,
          mode: body.mode === 'steer' ? 'steer' : 'queue',
          content: [{ type: 'text', text }],
        },
        new AbortController().signal,
      )
      sendJson(res, 200, { accepted: true, requestId: reqId })
    } catch (e) { fail(res, 500, String((e && e.message) || e)) }
  })

  route('/phone/v1/follow', async (req, res) => {
    if (!authed(req)) return fail(res, 401, 'unauthorized')
    const q = query(req)
    const sid = q.sessionId
    if (!sid) return fail(res, 400, 'sessionId required')
    res.writeHead(200, {
      'content-type': 'text/event-stream; charset=utf-8',
      'cache-control': 'no-cache',
      connection: 'keep-alive',
      'x-accel-buffering': 'no',
    })
    res.write(': open\n\n')
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
        res.write('data: ' + JSON.stringify(frame) + '\n\n')
      }
    } catch (e) {
      if (!closed) {
        res.write('event: error\ndata: ' + JSON.stringify({ message: String((e && e.message) || e) }) + '\n\n')
      }
    }
    if (!closed) res.end()
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

export { apply, inject, name }
