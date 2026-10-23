import type { RepoConfig } from '../../config.js'
import type { Clock, GitHubPort, Logger, RegistryPort } from '../../ports.js'
import type { StateStore } from '../../state/index.js'

export interface BumpedPackage {
  name: string
  /** null for a package the PR introduces. */
  from: string | null
  to: string
}

export interface BumpScan {
  packages: BumpedPackage[]
  /** package.json files whose version moved but whose name the diff did not show. */
  unresolved: string[]
}

export type ShipStatus =
  | 'nothing_to_ship'
  /** Dry run: stopped before the first GitHub write; `plan` says what it would do. */
  | 'planned'
  | 'shipped'
  | 'failed'
  | 'aborted'
  /** A wait ran out. The session stays resumable. */
  | 'timed_out'
  /** A run needs a human approval ground may not give. The session stays resumable. */
  | 'blocked'
  /** Release run is green but the registry never showed the version. Resumable. */
  | 'registry_pending'

export interface ShipPlan {
  action: 'approve' | 'rerun' | 'merge'
  runId?: number
  description: string
}

export interface ShipOutcome {
  status: ShipStatus
  /** Human-readable, printable as is. */
  summary: string
  sessionId: string
  prNumber: number | null
  plan?: ShipPlan
  packages: BumpedPackage[]
  mergeCommitSha?: string
  /** Whether the base branch was still the tip the PR was built on when merged. */
  baseWasTip?: boolean
  failedJobs?: string[]
  missingPackages?: BumpedPackage[]
  reruns: number
}

export interface ShipOptions {
  confirm: boolean
  /** How long to wait for the version PR's CI to finish, default 120. */
  ciWaitMinutes?: number
  /** How long to wait for the release run to appear and finish, default 90. */
  releaseWaitMinutes?: number
}

export interface ShipDeps {
  config: RepoConfig
  github: GitHubPort
  registry: RegistryPort
  clock: Clock
  logger: Logger
  store: StateStore
}
