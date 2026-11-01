import { EnvironmentId, ORCHESTRATION_PROTOCOL_VERSION } from "@t3tools/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";

import * as RpcHttp from "../rpc/http.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as Persistence from "../platform/persistence.ts";
import { BearerConnectionProfile, BearerConnectionRegistration } from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionDriver from "./driver.ts";
import { BearerConnectionTarget, ConnectionBlockedError } from "./model.ts";
import * as Onboarding from "./onboarding.ts";
import { prepareDirectRegistration, prepareHostConnectionUpdate } from "./onboarding.ts";
import * as EnvironmentRegistry from "./registry.ts";
import * as ConnectionProfileStore from "./profileStore.ts";
import * as ConnectionWakeups from "./wakeups.ts";

type Calls = Array<string>;

interface HostHttpOptions {
  readonly failDescriptor?: boolean;
  readonly protocolVersion?: number;
}

function layerHostHttp(calls: Calls, options?: HostHttpOptions) {
  const fetchFn = ((input) => {
    const url = String(input);
    calls.push(url);
    if (!url.endsWith("/.well-known/t3/environment")) {
      return Promise.reject(new Error(`Unexpected request: ${url}`));
    }
    if (options?.failDescriptor === true) {
      return Promise.resolve(Response.json({ message: "descriptor unavailable" }, { status: 503 }));
    }
    return Promise.resolve(
      Response.json({
        environmentId: "environment-shieldy",
        label: "shieldy",
        platform: { os: "linux", arch: "x64" },
        serverVersion: "0.0.0-test",
        orchestrationProtocolVersion: options?.protocolVersion ?? ORCHESTRATION_PROTOCOL_VERSION,
        capabilities: { repositoryIdentity: true },
      }),
    );
  }) satisfies typeof fetch;
  return RpcHttp.layerRemoteHttpClient(fetchFn);
}

function layerOnboarding(calls: Calls = [], options?: HostHttpOptions) {
  const dependencies = Layer.mergeAll(
    Layer.succeed(
      ClientCapabilities.ClientPresentation,
      ClientCapabilities.ClientPresentation.of({
        metadata: { label: "j3 Test", deviceType: "mobile", os: "ios" },
      }),
    ),
    layerHostHttp(calls, options),
    Layer.mock(Persistence.ConnectionTargetStore)({
      list: Effect.succeed([]),
      listDisabled: Effect.succeed([]),
    }),
    Layer.mock(Persistence.ConnectionRegistrationStore)({
      register: () => Effect.void,
      setEnabled: () => Effect.void,
    }),
    Layer.mock(Persistence.EnvironmentCacheStore)({}),
    Layer.mock(ConnectionProfileStore.ConnectionProfileStore)({}),
    Layer.succeed(
      Connectivity.Connectivity,
      Connectivity.Connectivity.of({ status: Effect.succeed("online"), changes: Stream.never }),
    ),
    Layer.mock(ConnectionDriver.ConnectionDriver)({ connect: () => Effect.never }),
    Layer.succeed(
      ConnectionWakeups.ConnectionWakeups,
      ConnectionWakeups.ConnectionWakeups.of({ changes: Stream.never }),
    ),
  );
  return Onboarding.layer.pipe(
    Layer.provideMerge(EnvironmentRegistry.layer.pipe(Layer.provideMerge(dependencies))),
  );
}

const ENVIRONMENT_ID = EnvironmentId.make("environment-shieldy");
const CONNECTION_ID = "bearer:environment-shieldy:http://shieldy:3780";
const PROFILE = new BearerConnectionProfile({
  connectionId: CONNECTION_ID,
  environmentId: ENVIRONMENT_ID,
  label: "shieldy",
  httpBaseUrl: "http://shieldy:3780/",
  wsBaseUrl: "ws://shieldy:3780/",
});
const UNSUPPORTED_ERROR = new ConnectionBlockedError({
  reason: "unsupported",
  detail: "The saved server used an incompatible orchestration protocol.",
});

describe("connection onboarding", () => {
  it.effect.each(["shieldy:3780", "http://shieldy:3780", "  http://shieldy:3780/some/path  "])(
    "prepares a host registration from %s",
    (host) =>
      Effect.gen(function* () {
        const calls: Calls = [];
        const registration = yield* prepareDirectRegistration({ host }).pipe(
          Effect.provide(layerOnboarding(calls)),
        );

        expect(registration).toEqual(
          new BearerConnectionRegistration({
            target: new BearerConnectionTarget({
              environmentId: ENVIRONMENT_ID,
              label: "shieldy",
              connectionId: CONNECTION_ID,
            }),
            profile: PROFILE,
          }),
        );
        expect(calls).toEqual(["http://shieldy:3780/.well-known/t3/environment"]);
      }),
  );

  it.effect("refuses an empty host before any request", () =>
    Effect.gen(function* () {
      const calls: Calls = [];
      const error = yield* prepareDirectRegistration({ host: "   " }).pipe(
        Effect.provide(layerOnboarding(calls)),
        Effect.flip,
      );

      expect(error).toMatchObject({ reason: "configuration", detail: "Enter a host URL." });
      expect(calls).toEqual([]);
    }),
  );

  it.effect.each([ORCHESTRATION_PROTOCOL_VERSION - 1, ORCHESTRATION_PROTOCOL_VERSION + 1])(
    "refuses a host speaking orchestration protocol %s",
    (protocolVersion) =>
      Effect.gen(function* () {
        const error = yield* prepareDirectRegistration({ host: "shieldy:3780" }).pipe(
          Effect.provide(layerOnboarding([], { protocolVersion })),
          Effect.flip,
        );

        expect(error).toMatchObject({ reason: "unsupported" });
      }),
  );

  it.effect("reports a host whose descriptor cannot be read", () =>
    Effect.gen(function* () {
      const error = yield* prepareDirectRegistration({ host: "shieldy:3780" }).pipe(
        Effect.provide(layerOnboarding([], { failDescriptor: true })),
        Effect.flip,
      );

      expect(error._tag).toBe("ConnectionTransientError");
    }),
  );

  it.effect("saves a host and turns a switched-off copy of it back on", () =>
    Effect.gen(function* () {
      const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
      const onboarding = yield* Onboarding.ConnectionOnboarding;
      yield* registry.register(
        new BearerConnectionRegistration({
          target: new BearerConnectionTarget({
            environmentId: ENVIRONMENT_ID,
            label: "Old label",
            connectionId: CONNECTION_ID,
          }),
          profile: new BearerConnectionProfile({ ...PROFILE, label: "Old label" }),
        }),
      );
      yield* registry.setEnabled(ENVIRONMENT_ID, false);
      yield* registry.setCompatibility(ENVIRONMENT_ID, UNSUPPORTED_ERROR);

      const environmentId = yield* onboarding.registerDirect({ host: "shieldy:3780" });

      expect(environmentId).toBe(ENVIRONMENT_ID);
      const entries = yield* SubscriptionRef.get(registry.entries);
      const entry = entries.get(environmentId);
      expect(entries.size).toBe(1);
      expect(entry?.enabled).toBe(true);
      expect(entry?.unsupportedReason).toBeUndefined();
      expect(entry?.profile).toEqual(Option.some(PROFILE));
    }).pipe(Effect.provide(layerOnboarding())),
  );

  it.effect("renames and moves a saved host while keeping its identity", () =>
    Effect.gen(function* () {
      const registration = yield* prepareHostConnectionUpdate({
        input: {
          environmentId: ENVIRONMENT_ID,
          label: "  Renamed host  ",
          httpBaseUrl: "http://100.65.180.100:3780/path",
        },
        entry: Option.some({
          target: new BearerConnectionTarget({
            environmentId: ENVIRONMENT_ID,
            label: "shieldy",
            connectionId: CONNECTION_ID,
          }),
          profile: Option.some(PROFILE),
          enabled: true,
        }),
      });

      expect(registration).toMatchObject({
        target: { environmentId: ENVIRONMENT_ID, label: "Renamed host", connectionId: CONNECTION_ID },
        profile: {
          environmentId: ENVIRONMENT_ID,
          label: "Renamed host",
          httpBaseUrl: "http://100.65.180.100:3780/",
          wsBaseUrl: "ws://100.65.180.100:3780/",
        },
      });
    }),
  );

  it.effect("refuses to edit an environment that is not saved, or to clear its label", () =>
    Effect.gen(function* () {
      const input = { environmentId: ENVIRONMENT_ID, label: "x", httpBaseUrl: "http://shieldy/" };
      expect(
        yield* Effect.flip(prepareHostConnectionUpdate({ input, entry: Option.none() })),
      ).toMatchObject({ reason: "configuration" });
      expect(
        yield* Effect.flip(
          prepareHostConnectionUpdate({
            input: { ...input, label: "  " },
            entry: Option.some({
              target: new BearerConnectionTarget({
                environmentId: ENVIRONMENT_ID,
                label: "shieldy",
                connectionId: CONNECTION_ID,
              }),
              profile: Option.some(PROFILE),
              enabled: true,
            }),
          }),
        ),
      ).toMatchObject({ detail: "Environment label cannot be empty." });
    }),
  );
});
