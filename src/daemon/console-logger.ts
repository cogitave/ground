import type { Clock, Logger } from '../ports.js'

type Level = 'debug' | 'info' | 'warn' | 'error'

/** One line per entry on stderr, so stdout stays clean for command output and journald picks it up. */
export class ConsoleLogger implements Logger {
  constructor(
    private readonly clock: Clock,
    private readonly write: (line: string) => void = (l) => process.stderr.write(`${l}\n`),
    private readonly verbose = false,
  ) {}

  debug(msg: string, data?: Record<string, unknown>): void {
    if (this.verbose) this.emit('debug', msg, data)
  }
  info(msg: string, data?: Record<string, unknown>): void {
    this.emit('info', msg, data)
  }
  warn(msg: string, data?: Record<string, unknown>): void {
    this.emit('warn', msg, data)
  }
  error(msg: string, data?: Record<string, unknown>): void {
    this.emit('error', msg, data)
  }

  private emit(level: Level, msg: string, data?: Record<string, unknown>): void {
    const tail = data ? ` ${JSON.stringify(data)}` : ''
    this.write(`${this.clock.now().toISOString()} ${level.toUpperCase()} ${msg}${tail}`)
  }
}
