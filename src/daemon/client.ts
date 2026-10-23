import { createConnection } from 'node:net'

/** Sends one JSON-lines request and resolves with the one-line reply. Rejects if nobody listens. */
export function socketRequest(path: string, request: unknown): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const sock = createConnection(path)
    let buf = ''
    sock.setEncoding('utf8')
    sock.once('error', reject)
    sock.once('connect', () => sock.write(`${JSON.stringify(request)}\n`))
    sock.on('data', (chunk: string) => {
      buf += chunk
      const nl = buf.indexOf('\n')
      if (nl < 0) return
      sock.end()
      try {
        resolve(JSON.parse(buf.slice(0, nl)))
      } catch (err) {
        reject(err)
      }
    })
    sock.once('close', () => reject(new Error('socket closed without a reply')))
  })
}
