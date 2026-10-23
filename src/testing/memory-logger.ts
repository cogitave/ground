import type { Logger } from '../ports.js'

export interface LogEntry {
  level: 'debug' | 'info' | 'warn' | 'error'
  msg: string
  data?: Record<string, unknown>
}

export class MemoryLogger implements Logger {
  readonly entries: LogEntry[] = []

  debug(msg: string, data?: Record<string, unknown>): void {
    this.push('debug', msg, data)
  }
  info(msg: string, data?: Record<string, unknown>): void {
    this.push('info', msg, data)
  }
  warn(msg: string, data?: Record<string, unknown>): void {
    this.push('warn', msg, data)
  }
  error(msg: string, data?: Record<string, unknown>): void {
    this.push('error', msg, data)
  }

  messages(level?: LogEntry['level']): string[] {
    return this.entries.filter((e) => !level || e.level === level).map((e) => e.msg)
  }

  private push(level: LogEntry['level'], msg: string, data?: Record<string, unknown>): void {
    this.entries.push(data ? { level, msg, data } : { level, msg })
  }
}
