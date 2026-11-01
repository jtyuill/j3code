import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";

/** A step's failure, already formatted for the report. */
export class StepFailure extends Error {
  override readonly name = "StepFailure";
}

export const fail = (message: string): never => {
  throw new StepFailure(message);
};

export function check(condition: boolean, message: string): asserts condition {
  if (!condition) throw new StepFailure(message);
}

const MAX_DEPTH = 6;

/**
 * Renders an error, defect or typed failure with its nested `reason`/`cause` chain. Only scalar
 * fields are shown, which keeps typed RPC errors (`commandId`, `detail`, …) and drops request dumps.
 */
export function formatError(error: unknown, seen = new Set<unknown>()): string {
  if (seen.size > MAX_DEPTH) return "…";
  if (Schema.isSchemaError(error)) {
    return `SchemaError (contracts schema rejected the value):\n${indent(error.message)}`;
  }
  if (error === null || typeof error !== "object") return String(error);
  seen.add(error);
  const record = error as Record<string, unknown>;
  const tag = typeof record._tag === "string" ? record._tag : undefined;
  const name = tag ?? (error instanceof Error ? error.name : "Error");
  const message = typeof record.message === "string" && record.message !== "" ? record.message : undefined;
  const scalars = Object.entries(record).filter(
    ([key, value]) =>
      !["_tag", "message", "stack", "name"].includes(key) &&
      !key.startsWith("~") &&
      (value === null || ["string", "number", "boolean"].includes(typeof value)),
  );
  let text = message === undefined ? name : `${name}: ${message}`;
  if (scalars.length > 0) text += ` ${safeJson(Object.fromEntries(scalars))}`;
  if (!(error instanceof Error) && tag === undefined && message === undefined) {
    const json = safeJson(error);
    const kind: string = Object.getPrototypeOf(error)?.constructor?.name ?? "Object";
    text = json === "{}" ? `${kind} (no detail)` : json;
  }
  for (const key of ["reason", "cause"] as const) {
    const nested = record[key];
    if (nested !== undefined && nested !== null && !seen.has(nested)) {
      text += `\n${key}: ${formatError(nested, seen)}`;
    }
  }
  return text;
}

/** Renders an Effect failure cause: typed failures, defects (incl. decode errors) and interrupts. */
export function formatCause(cause: Cause.Cause<unknown>): string {
  const parts = cause.reasons.map((reason) => {
    if (Cause.isFailReason(reason)) return `typed failure: ${formatError(reason.error)}`;
    if (Cause.isDieReason(reason)) return `defect: ${formatError(reason.defect)}`;
    return "interrupted";
  });
  return parts.length === 0 ? "empty cause" : parts.join("\n");
}

export function indent(text: string, prefix = "    "): string {
  return text
    .split("\n")
    .map((line) => prefix + line)
    .join("\n");
}

function safeJson(value: unknown): string {
  try {
    const text = JSON.stringify(value);
    return text.length > 2000 ? `${text.slice(0, 2000)}…` : text;
  } catch {
    return String(value);
  }
}

type Outcome = "PASS" | "FAIL" | "SKIP";

/**
 * Runs steps in order and prints one line per step. A step whose prerequisites did not pass is
 * skipped, so one failure does not cascade into a wall of follow-on failures.
 */
export class Runner {
  private readonly outcomes = new Map<string, Outcome>();

  async step(
    id: string,
    title: string,
    requires: ReadonlyArray<string>,
    body: () => Promise<string | void>,
  ): Promise<boolean> {
    const missing = requires.filter((dep) => this.outcomes.get(dep) !== "PASS");
    if (missing.length > 0) {
      this.record(id, "SKIP", title, `needs ${missing.join(", ")}`);
      return false;
    }
    const started = performance.now();
    try {
      const detail = await body();
      const elapsed = `${((performance.now() - started) / 1000).toFixed(1)}s`;
      this.record(id, "PASS", title, detail ? `${detail} (${elapsed})` : elapsed);
      return true;
    } catch (error) {
      const message = error instanceof StepFailure ? error.message : formatError(error);
      this.record(id, "FAIL", title, undefined);
      process.stdout.write(`${indent(message)}\n`);
      return false;
    }
  }

  passed(id: string): boolean {
    return this.outcomes.get(id) === "PASS";
  }

  summary(): { pass: number; fail: number; skip: number } {
    const counts = { pass: 0, fail: 0, skip: 0 };
    for (const outcome of this.outcomes.values()) {
      if (outcome === "PASS") counts.pass += 1;
      else if (outcome === "FAIL") counts.fail += 1;
      else counts.skip += 1;
    }
    return counts;
  }

  private record(id: string, outcome: Outcome, title: string, detail: string | undefined): void {
    this.outcomes.set(id, outcome);
    const suffix = detail === undefined ? "" : ` — ${detail}`;
    process.stdout.write(`${outcome}  ${id.padEnd(24)} ${title}${suffix}\n`);
  }
}
