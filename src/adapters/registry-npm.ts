import type { RegistryPort } from '../ports.js'

export function parseLatest(body: unknown): string | null {
  if (typeof body !== 'object' || body === null) return null
  const tags = (body as { 'dist-tags'?: unknown })['dist-tags']
  if (typeof tags !== 'object' || tags === null) return null
  const latest = (tags as { latest?: unknown }).latest
  return typeof latest === 'string' ? latest : null
}

export function registryUrl(pkg: string, base = 'https://registry.npmjs.org'): string {
  // Scoped names keep the '@' and escape only the slash, as npm does.
  return `${base}/${pkg.replace('/', '%2F')}`
}

export class NpmRegistry implements RegistryPort {
  constructor(private readonly fetchFn: typeof fetch = fetch) {}

  async latestVersion(pkg: string): Promise<string | null> {
    const res = await this.fetchFn(registryUrl(pkg), {
      headers: { accept: 'application/vnd.npm.install-v1+json' },
    })
    if (res.status === 404) return null
    if (!res.ok) throw new Error(`npm registry answered ${res.status} for ${pkg}`)
    return parseLatest(await res.json())
  }
}
