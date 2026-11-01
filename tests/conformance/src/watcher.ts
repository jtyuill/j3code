import { StepFailure } from "./report.ts";

type End = { readonly ok: true } | { readonly ok: false; readonly error: string };

/** The item-type-independent face of a watcher, for code that only waits on derived state. */
export type Waiter = Pick<Watcher<unknown>, "until" | "ended" | "label">;

/**
 * Collects the decoded items of one RPC stream and lets steps wait for a condition on the state
 * derived from them. Conditions are re-evaluated synchronously after every item, so transient
 * states are not missed as long as the wait starts before they occur.
 */
export class Watcher<A> {
  readonly items: A[] = [];
  private end: End | undefined;
  private readonly listeners = new Set<() => void>();

  readonly label: string;
  private readonly onItem: (item: A) => void;
  private readonly describe: () => string;

  constructor(label: string, onItem: (item: A) => void, describe: () => string) {
    this.label = label;
    this.onItem = onItem;
    this.describe = describe;
  }

  push(item: A): void {
    this.items.push(item);
    this.onItem(item);
    this.notify();
  }

  finish(end: End): void {
    if (this.end !== undefined) return;
    this.end = end;
    this.notify();
  }

  get ended(): End | undefined {
    return this.end;
  }

  /**
   * Resolves with the first non-undefined result of `probe`, or fails on timeout or stream end.
   * The probe also re-runs every second, so it may enforce its own grace periods.
   */
  until<T>(what: string, timeoutMs: number, probe: () => T | undefined): Promise<T> {
    const { promise, resolve, reject } = Promise.withResolvers<T>();
    const done = (settle: () => void) => {
      clearTimeout(timer);
      clearInterval(tick);
      this.listeners.delete(listener);
      settle();
    };
    const listener = () => {
      let value: T | undefined;
      try {
        value = probe();
      } catch (error) {
        done(() => reject(error));
        return;
      }
      if (value !== undefined) {
        const found = value;
        done(() => resolve(found));
        return;
      }
      if (this.end !== undefined) {
        const reason = this.end.ok ? "the stream ended" : `the stream failed:\n${this.end.error}`;
        const message = `${this.label}: ${reason}\nwhile waiting for ${what}\nstate: ${this.describe()}`;
        done(() => reject(new StepFailure(message)));
      }
    };
    const timer = setTimeout(() => {
      const message = `${this.label}: timed out after ${timeoutMs / 1000}s waiting for ${what}\nstate: ${this.describe()}`;
      done(() => reject(new StepFailure(message)));
    }, timeoutMs);
    const tick = setInterval(listener, 1000);
    this.listeners.add(listener);
    listener();
    return promise;
  }

  private notify(): void {
    for (const listener of [...this.listeners]) listener();
  }
}
