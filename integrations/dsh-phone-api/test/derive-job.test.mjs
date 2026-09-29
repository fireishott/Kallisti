// node --test ~/.dsh/plugins/dsh-phone-api/test/derive-job.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { deriveJob } from '../lib/index.js'

const ev = (seq, type, data = {}) => ({ seq, type, time: 1000 + seq, data })
const user = (seq, rpcId, text = 'hi') =>
  ev(seq, 'user/message', { content: [{ type: 'text', text }], source: { kind: 'user', rpcId } })
const spliced = (seq, rpcId) =>
  ev(seq, 'agent/inbox/spliced', { inserted: [{ source: { kind: 'user', rpcId } }] })
const answer = (seq, text, id = 'm' + seq) =>
  ev(seq, 'assistant/message', { message: { id, content: [{ type: 'text', text }], source: { kind: 'model' } } })

test('completed turn returns the last model answer', () => {
  const events = [spliced(1, 'kallisti-a'), ev(2, 'turn/start', { turn: 1 }), user(3, 'kallisti-a'),
    answer(4, ''), answer(5, 'done'), ev(6, 'turn/end', { turn: 1, reason: { kind: 'completed' } })]
  const j = deriveJob(events, 'kallisti-a', false)
  assert.equal(j.status, 'completed'); assert.equal(j.text, 'done'); assert.equal(j.messageId, 'm5')
})

test('interrupted turn (the 10:40 host restart) is interrupted, not failed', () => {
  const events = [ev(1, 'turn/start', { turn: 1 }), user(2, 'kallisti-a'),
    ev(3, 'turn/end', { turn: 1, reason: { kind: 'interrupted' } })]
  assert.equal(deriveJob(events, 'kallisti-a', false).status, 'interrupted')
})

test('hard kill with no turn/end and session idle is interrupted', () => {
  const events = [ev(1, 'turn/start', { turn: 1 }), user(2, 'kallisti-a'), answer(3, 'partial')]
  assert.equal(deriveJob(events, 'kallisti-a', false).status, 'interrupted')
  assert.equal(deriveJob(events, 'kallisti-a', true).status, 'running')
})

test('queued behind another turn', () => {
  const events = [ev(1, 'turn/start', { turn: 1 }), user(2, 'kallisti-x'), spliced(3, 'kallisti-a')]
  assert.equal(deriveJob(events, 'kallisti-a', true).status, 'queued')
})

test('a later turn ending does not settle an earlier running one', () => {
  const events = [ev(1, 'turn/start', { turn: 1 }), user(2, 'kallisti-a'),
    ev(3, 'turn/end', { turn: 2, reason: { kind: 'completed' } })]
  assert.equal(deriveJob(events, 'kallisti-a', true).status, 'running')
})

test('error and abort map to failed and cancelled', () => {
  const base = [ev(1, 'turn/start', { turn: 1 }), user(2, 'kallisti-a')]
  const err = deriveJob([...base, ev(3, 'turn/end', { turn: 1, reason: { kind: 'error', error: { message: 'boom', code: 'RATE_LIMIT' } } })], 'kallisti-a', false)
  assert.equal(err.status, 'failed'); assert.equal(err.errorCode, 'RATE_LIMIT')
  const ab = deriveJob([...base, ev(3, 'turn/end', { turn: 1, reason: { kind: 'aborted' } })], 'kallisti-a', false)
  assert.equal(ab.status, 'cancelled')
})

test('unknown request is null', () => {
  assert.equal(deriveJob([user(1, 'kallisti-z')], 'kallisti-a', false), null)
})
