// Signed media URL support for the Kallisti iOS client.
//
// DSH's app bridge fetches this endpoint as a real MessageAttachment and
// sends its existing bearer token. Signed URLs remain available for a
// deliberately short-lived fallback link, but DSH's cookie-gated `/api/file`
// route is never involved.
//
// URL shape: `/phone/v1/media?p=<urlencoded absolute path>` with either an
// Authorization bearer, or `exp=<epoch ms>&sig=<HMAC>`.


import { createHmac, timingSafeEqual } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { stat } from 'node:fs/promises'
import { extname } from 'node:path'

/** Media types Kallisti can decode, plus the usual web formats. */
const MIME_BY_EXTENSION = {
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.gif': 'image/gif',
  '.heic': 'image/heic',
  '.svg': 'image/svg+xml',
  '.mp4': 'video/mp4',
  '.mov': 'video/quicktime',
  '.pdf': 'application/pdf',
  '.txt': 'text/plain; charset=utf-8',
  '.md': 'text/markdown; charset=utf-8',
}

const MAX_BYTES = 24 * 1024 * 1024
const DEFAULT_TTL_MS = 30 * 60 * 1000

export function mediaTypeFor(path) {
  return MIME_BY_EXTENSION[extname(path).toLowerCase()] || 'application/octet-stream'
}

export function signMediaPath(path, expiresAt, secret) {
  return createHmac('sha256', secret).update(path + '\n' + String(expiresAt)).digest('hex')
}

/** Build the query string (without the leading `?`) for one signed media URL. */
export function buildMediaQuery(path, secret, { ttlMs = DEFAULT_TTL_MS, now = Date.now() } = {}) {
  const exp = now + ttlMs
  return 'p=' + encodeURIComponent(path) + '&exp=' + exp + '&sig=' + signMediaPath(path, exp, secret)
}

function equalConstantTime(a, b) {
  const ab = Buffer.from(String(a), 'utf8')
  const bb = Buffer.from(String(b), 'utf8')
  if (ab.length !== bb.length) return false
  return timingSafeEqual(ab, bb)
}

function reject(res, status, message) {
  const body = message + '\n'
  res.writeHead(status, {
    'content-type': 'text/plain; charset=utf-8',
    'content-length': Buffer.byteLength(body),
    'cache-control': 'no-store',
  })
  res.end(body)
}

/**
 * Create the `GET|HEAD /phone/v1/media` handler.
 *
 * Access is granted by a valid unexpired signature, or by the plugin bearer
 * token (so the route is still exercisable with curl during diagnosis).
 */
export function createMediaHandler({ secret, token, maxBytes = MAX_BYTES }) {
  return async function mediaHandler(req, res) {
    if (req.method !== 'GET' && req.method !== 'HEAD') return reject(res, 405, 'GET or HEAD required')

    let url
    try {
      url = new URL(req.url || '/', 'http://dsh.invalid')
    } catch {
      return reject(res, 400, 'bad request')
    }

    const path = url.searchParams.get('p') || ''
    const exp = Number(url.searchParams.get('exp') || 0)
    const sig = url.searchParams.get('sig') || ''
    const bearer = req.headers && req.headers['authorization'] === 'Bearer ' + token

    const signatureValid =
      path !== '' &&
      Number.isFinite(exp) &&
      exp > Date.now() &&
      sig !== '' &&
      equalConstantTime(sig, signMediaPath(path, exp, secret))

    if (!bearer && !signatureValid) {
      return reject(res, 401, exp > 0 && exp <= Date.now() ? 'link expired' : 'unauthorized')
    }

    if (!path.startsWith('/') || path.startsWith('//') || path.includes('\0')) {
      return reject(res, 400, 'absolute path required')
    }

    let info
    try {
      info = await stat(path)
    } catch {
      return reject(res, 404, 'not found')
    }
    if (!info.isFile()) return reject(res, 403, 'not a regular file')
    if (info.size > maxBytes) return reject(res, 413, 'file exceeds byte limit')

    const headers = {
      'content-type': mediaTypeFor(path),
      'content-length': String(info.size),
      'cache-control': 'private, max-age=300',
      'x-content-type-options': 'nosniff',
    }
    if (req.method === 'HEAD') {
      res.writeHead(200, headers)
      return res.end()
    }

    res.writeHead(200, headers)
    const stream = createReadStream(path)
    stream.on('error', () => res.destroy())
    res.on('close', () => stream.destroy())
    stream.pipe(res)
  }
}
