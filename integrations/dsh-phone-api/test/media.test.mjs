import { test } from "node:test"
import assert from "node:assert/strict"
import { createServer } from "node:http"
import { mkdtemp, writeFile, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createMediaHandler, buildMediaQuery } from "../lib/media.js"

test("media endpoint serves signed and bearer-authenticated image requests", async () => {
  const dir = await mkdtemp(join(tmpdir(), "dsh-media-"))
  const image = join(dir, "poster.jpg")
  await writeFile(image, Buffer.from([0xff, 0xd8, 0xff, 0xd9]))
  const secret = "test-secret"
  const server = createServer(createMediaHandler({ secret, token: "test-token" }))
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve))
  const port = server.address().port
  try {
    const url = `http://127.0.0.1:${port}/phone/v1/media?${buildMediaQuery(image, secret, { ttlMs: 60_000 })}`
    const okay = await fetch(url)
    assert.equal(okay.status, 200)
    assert.equal(okay.headers.get("content-type"), "image/jpeg")
    assert.deepEqual(Buffer.from(await okay.arrayBuffer()), Buffer.from([0xff, 0xd8, 0xff, 0xd9]))

    const tampered = await fetch(url.replace("poster.jpg", "other.jpg"))
    assert.equal(tampered.status, 401)

    // This is the actual Kallisti flow. DSHClient builds /phone/v1/media?p=…
    // and AttachmentService supplies the DSH bearer, not a signed URL.
    const bearer = await fetch(`http://127.0.0.1:${port}/phone/v1/media?p=${encodeURIComponent(image)}`, {
      headers: { authorization: "Bearer test-token" }
    })
    assert.equal(bearer.status, 200)
    assert.equal(bearer.headers.get("content-type"), "image/jpeg")
  } finally {
    await new Promise(resolve => server.close(resolve))
    await rm(dir, { recursive: true, force: true })
  }
})
