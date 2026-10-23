import type { RegistryPort } from '../ports.js'

export class FakeRegistry implements RegistryPort {
  readonly queries: string[] = []
  private readonly versions = new Map<string, string>()
  private readonly scheduled = new Map<string, { after: number; version: string }>()

  /** Publish immediately. */
  setVersion(pkg: string, version: string): this {
    this.versions.set(pkg, version)
    return this
  }

  /** Simulate propagation lag: `version` becomes visible on the (after+1)th query of `pkg`. */
  publishAfterQueries(pkg: string, version: string, after: number): this {
    this.scheduled.set(pkg, { after, version })
    return this
  }

  async latestVersion(pkg: string): Promise<string | null> {
    this.queries.push(pkg)
    const s = this.scheduled.get(pkg)
    if (s) {
      if (s.after <= 0) {
        this.versions.set(pkg, s.version)
        this.scheduled.delete(pkg)
      } else {
        s.after -= 1
      }
    }
    return this.versions.get(pkg) ?? null
  }
}
