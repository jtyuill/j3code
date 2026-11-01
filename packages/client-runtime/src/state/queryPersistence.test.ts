import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId } from "@t3tools/contracts";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { EnvironmentQueryStore, persistQuery } from "./queryPersistence.ts";

const env = EnvironmentId.make("one");
const other = EnvironmentId.make("two");
const persisted = persistQuery(Schema.Struct({ title: Schema.String }));

function store(records = new Map<string, string>()) {
  return {
    load: (environmentId: EnvironmentId, key: string) =>
      Effect.sync(() => Option.fromUndefinedOr(records.get(`${environmentId}:${key}`))),
    save: (environmentId: EnvironmentId, key: string, payload: string) =>
      Effect.sync(() => {
        records.set(`${environmentId}:${key}`, payload);
      }),
  };
}

describe("query persistence", () => {
  it.effect("round trips responses without mixing environments or query scopes", () =>
    Effect.gen(function* () {
      yield* persisted.save(env, "open", { title: "saved" });
      expect(yield* persisted.load(env, "open")).toEqual(Option.some({ title: "saved" }));
      expect(yield* persisted.load(other, "open")).toEqual(Option.none());
      expect(yield* persisted.load(env, "closed")).toEqual(Option.none());
    }).pipe(Effect.provideService(EnvironmentQueryStore, store())),
  );

  it.effect("ignores corrupt or incompatible records", () =>
    Effect.gen(function* () {
      expect(yield* persisted.load(env, "broken")).toEqual(Option.none());
      expect(yield* persisted.load(env, "incompatible")).toEqual(Option.none());
    }).pipe(
      Effect.provideService(
        EnvironmentQueryStore,
        store(
          new Map([
            ["one:broken", "not json"],
            ["one:incompatible", '{"title":42}'],
          ]),
        ),
      ),
    ),
  );
});
