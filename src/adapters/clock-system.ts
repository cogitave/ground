import type { Clock } from '../ports.js'

export class SystemClock implements Clock {
  now(): Date {
    return new Date()
  }

  sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms))
  }
}
