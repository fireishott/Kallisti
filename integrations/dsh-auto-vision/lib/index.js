// dsh-auto-vision — never fail an image prompt for lack of a vision model.
//
// DSH's session controller rejects a prompt that carries an image when the
// session's *current* model does not declare image input:
//
//   session/attachment-invalid  "Model X does not support image input."
//   details.reason === 'MODEL_DOES_NOT_SUPPORT_IMAGES'
//
// That is a dead end for the user: the image is already attached, and the only
// recovery is to know which of the configured models is vision-capable and
// switch by hand. This plugin wraps `sessionController.prompt` and, when an
// image prompt is refused for that reason (or the selected vision route is
// rate-limited), selects the next vision-capable model and admits the prompt
// again — so the turn just runs.
//
// Retrying is safe: these errors are thrown during *admission*, before any turn
// executes, so no work is duplicated. Once a prompt is admitted `prompt`
// resolves and the wrapper stops.

const name = 'dsh-auto-vision'

// `llm` discovers capability; `sessionController` is wrapped.
const inject = ['llm', 'sessionController']

// Vision routes that have been verified to read images on this deployment, in
// preference order. Discovery by capability is the fallback, so this list only
// decides *which* vision model wins, never whether one can be found.
const PREFERRED = [
  'cmc/moonshotai/Kimi-K2.7-Code',
  'cx/gpt-5.6-terra',
  'cx/gpt-5.6-sol',
  'cc/claude-sonnet-5',
  'cc/claude-opus-5',
  'cx/gpt-5.4',
  'MM/minimax-m3',
]

// Bounded so a deployment where every vision route is down still terminates
// quickly with the original error instead of cycling the whole catalog.
const MAX_ATTEMPTS = 4

function isImageUnsupported(error) {
  if (!error || typeof error !== 'object') return false
  const details = error.details || error.data || {}
  if (details && details.reason === 'MODEL_DOES_NOT_SUPPORT_IMAGES') return true
  const message = typeof error.message === 'string' ? error.message : ''
  return message.includes('does not support image input')
}

// Rate-limited / unavailable route. Only consulted for image prompts, and only
// during admission, so switching models here cannot abandon running work.
function isRouteUnavailable(error) {
  if (!error || typeof error !== 'object') return false
  const message = `${error.message || ''} ${error.code || ''}`
  return /429|rate.?limit|usage limit|503|overloaded|no longer available/i.test(message)
}

function hasImage(content) {
  return Array.isArray(content) && content.some((part) => part && part.type === 'image')
}

async function supportsImage(ctx, provider, model) {
  try {
    const info = await ctx.llm.resolveModelInfo(provider, model)
    return info && Array.isArray(info.inputModalities) && info.inputModalities.includes('image')
  } catch {
    return false
  }
}

/**
 * Rank the configured routes by whether they can take an image.
 *
 * Preferred ids win first (still capability-checked, so a stale id can never
 * be selected), then every other image-capable route in catalog order. Returns
 * `[]` when the deployment genuinely has no vision model, which leaves the
 * original DSH error untouched rather than inventing a fallback.
 */
async function visionCandidates(ctx, exclude) {
  const found = []
  let providers = []
  try {
    providers = ctx.llm.listProviders() || []
  } catch {
    return []
  }
  for (const provider of providers) {
    let models = []
    try {
      models = (await ctx.llm.listModels(provider.id)) || []
    } catch {
      continue
    }
    for (const model of models) {
      if (!(await supportsImage(ctx, provider.id, model.id))) continue
      const id = `${provider.id}/${model.id}`
      if (exclude.has(id)) continue
      found.push({ provider: provider.id, model: model.id, id })
    }
  }
  const rank = (entry) => {
    const index = PREFERRED.indexOf(entry.model)
    return index === -1 ? PREFERRED.length : index
  }
  return found.sort((a, b) => rank(a) - rank(b))
}

function apply(ctx) {
  const sc = ctx.sessionController
  if (!sc || typeof sc.prompt !== 'function') return

  const originalPrompt = sc.prompt.bind(sc)

  sc.prompt = async (request, signal) => {
    if (!hasImage(request && request.content)) return originalPrompt(request, signal)

    const tried = new Set()
    // The session's existing choice is recorded so a later attempt never
    // re-selects the model that just failed.
    try {
      const current = await currentSelectionOf(ctx, sc, request.sessionId)
      if (current && current.provider && current.model) tried.add(`${current.provider}/${current.model}`)
    } catch {
      /* unknown current selection is fine — the first error still tells us */
    }

    const candidates = await visionCandidates(ctx, tried)
    let lastError

    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt += 1) {
      try {
        return await originalPrompt(request, signal)
      } catch (error) {
        lastError = error
        const recoverable = isImageUnsupported(error) || isRouteUnavailable(error)
        if (!recoverable) throw error

        const next = candidates.shift()
        if (!next) throw error

        try {
          await sc.selectModel({
            sessionId: request.sessionId,
            provider: next.provider,
            model: next.model,
          })
          ctx.logger?.info?.(
            'dsh-auto-vision: image prompt refused, switched %s to %s/%s',
            request.sessionId,
            next.provider,
            next.model,
          )
        } catch (selectionError) {
          // A route that cannot even be selected is not a route; try the next.
          lastError = selectionError
        }
      }
    }

    throw lastError
  }
}

/**
 * Best-effort read of the session's current model.
 *
 * The public controller surface has no plain "current selection" getter, so the
 * value is read from the session projection when the page API exposes it and is
 * otherwise treated as unknown. Only used to avoid re-selecting a model that
 * already failed; correctness never depends on it.
 */
async function currentSelectionOf(ctx, sc, sessionId) {
  try {
    const page = await sc.page({ sessionId, maxMessages: 1 })
    const direct = page && page.projections && page.projections.values && page.projections.values.modelSelection
    if (direct) {
      const chosen = direct.next || direct.lastUsed
      if (chosen && chosen.provider && chosen.model) return chosen
    }
  } catch {
    /* fall through to the configured default */
  }
  try {
    const fallback = ctx.agentDefaultModel && ctx.agentDefaultModel.currentSelection
      ? ctx.agentDefaultModel.currentSelection()
      : undefined
    if (fallback && fallback.provider && fallback.model) return fallback
  } catch {
    /* no default available */
  }
  return undefined
}

export { name, inject, apply }
