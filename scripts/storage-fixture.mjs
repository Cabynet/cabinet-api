// Transport fixture, not an S3 signature verifier. Never run as production storage.
import { createServer } from "node:http"
const objects = new Map()
const server = createServer(async (req, res) => {
  res.setHeader("Access-Control-Allow-Origin", process.env.TEST_WEB_ORIGIN || "http://localhost:3300")
  res.setHeader("Access-Control-Allow-Methods", "PUT, HEAD, GET, DELETE, OPTIONS")
  res.setHeader("Access-Control-Allow-Headers", "Content-Type")
  if (req.method === "OPTIONS") { res.writeHead(204); res.end(); return }
  const url = new URL(req.url, "http://localhost")
  if ((req.method === "PUT" || req.method === "HEAD" || req.method === "DELETE") && !/^[a-f0-9]{64}$/.test(url.searchParams.get("X-Amz-Signature") || "")) { res.writeHead(403); res.end(); return }
  if (req.method === "PUT") {
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    objects.set(url.pathname, { body: Buffer.concat(chunks), type: req.headers["content-type"] || "application/octet-stream" })
    res.writeHead(200); res.end(); return
  }
  if (req.method === "DELETE") { objects.delete(url.pathname); res.writeHead(204); res.end(); return }
  const object = objects.get(url.pathname)
  if (!object) { res.writeHead(404); res.end(); return }
  res.setHeader("Content-Length", object.body.length)
  res.setHeader("Content-Type", object.type)
  res.writeHead(200); res.end(req.method === "HEAD" ? undefined : object.body)
})
server.listen(Number(process.env.TEST_STORAGE_PORT || 59001), "127.0.0.1", () => console.log(`STORAGE_PORT=${server.address().port}`))
