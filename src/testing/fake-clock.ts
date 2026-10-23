import type { Clock } from '../ports.js'

/** Virtual time: sleep returns at once, advancing the clock and recording the request. */
export class FakeClock implements Clock {
  readonly sleeps: number[] = []
  private ms: number

  constructor(start: Date | string = '2026-01-01T00:00:00.000Z') {
    this.ms = new Date(start).getTime()
  }

  now(): Date {
    return new Date(this.ms)
  }

  async sleep(ms: number): Promise<void> {
    this.sleeps.push(ms)
    this.ms += ms
  }

  advance(ms: number): void {
    this.ms += ms
  }

  get totalSlept(): number {
    return this.sleeps.reduce((a, b) => a + b, 0)
  }
}
