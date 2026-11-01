import {
  EnvironmentId,
  type OrchestrationV2ShellSnapshot,
  type ServerConfig,
} from "@t3tools/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Context from "effect/Context";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Scheduler from "effect/Scheduler";
import * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as Tracer from "effect/Tracer";

import {
  BearerConnectionProfile,
  BearerConnectionRegistration,
  type ConnectionCatalogEntry,
  type ConnectionRegistration,
  type ConnectionRoute,
  PrimaryConnectionRegistration,
  type ConnectionProfile,
} from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionDriver from "./driver.ts";
import type { RouteCheck } from "./driver.ts";
import { connectionRouteId, connectionRouteKind } from "./routes.ts";
import {
  ConnectionTransientError,
  ConnectionBlockedError,
  BearerConnectionTarget,
  PrimaryConnectionTarget,
  type ConnectionTarget,
  type PreparedConnection,
  type SupervisorConnectionState,
} from "./model.ts";
import * as Persistence from "../platform/persistence.ts";
import * as ConnectionProfileStore from "./profileStore.ts";
import * as EnvironmentRegistry from "./registry.ts";
import * as RpcSession from "../rpc/session.ts";
import * as EnvironmentSupervisor from "./supervisor.ts";
import * as ConnectionWakeups from "./wakeups.ts";
import { runDesktopCommitWithReconnectObserver } from "../state/server.ts";
import { v2ShellSnapshot } from "../state/orchestrationV2TestFixtures.ts";

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});
const SECOND_TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-2"),
  label: "Second environment",
  httpBaseUrl: "https://environment-2.example.test",
  wsBaseUrl: "wss://environment-2.example.test",
});

const PREPARED: PreparedConnection = {
  environmentId: TARGET.environmentId,
  label: TARGET.label,
  httpBaseUrl: TARGET.httpBaseUrl,
  socketUrl: "wss://environment.example.test/ws",
  target: TARGET,
};

const HOST_TARGET = new BearerConnectionTarget({
  environmentId: EnvironmentId.make("environment-host"),
  label: "Host environment",
  connectionId: "host-connection",
});
const HOST_PROFILE = new BearerConnectionProfile({
  connectionId: HOST_TARGET.connectionId,
  environmentId: HOST_TARGET.environmentId,
  label: HOST_TARGET.label,
  httpBaseUrl: "https://host.example.test",
  wsBaseUrl: "wss://host.example.test",
});

const BEARER_TARGET = new BearerConnectionTarget({
  environmentId: EnvironmentId.make("environment-bearer"),
  label: "Bearer environment",
  connectionId: "bearer-connection",
});
const BEARER_PROFILE = new BearerConnectionProfile({
  connectionId: BEARER_TARGET.connectionId,
  environmentId: BEARER_TARGET.environmentId,
  label: BEARER_TARGET.label,
  httpBaseUrl: "https://bearer.example.test",
  wsBaseUrl: "wss://bearer.example.test",
});

const CACHED_SNAPSHOT: OrchestrationV2ShellSnapshot = {
  ...v2ShellSnapshot,
  snapshotSequence: 1,
};

interface SessionControl {
  readonly closed: Deferred.Deferred<never, ConnectionTransientError>;
}

const makeHarness = Effect.fn("TestEnvironmentRegistry.makeHarness")(function* (
  initialTargets: ReadonlyArray<ConnectionTarget>,
  initialProfiles: ReadonlyArray<ConnectionProfile> = [HOST_PROFILE],
  options?: {
    readonly prepareError?: ConnectionBlockedError;
    readonly beforeSessionConnect?: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly beforeRegistrationRegister?: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<void, Persistence.ConnectionPersistenceError>;
    readonly beforeRegistrationRemove?: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<void, Persistence.ConnectionPersistenceError>;
    readonly checkRoute?: (route: ConnectionRoute) => RouteCheck;
    readonly prepareRoute?: (target: ConnectionTarget) => ConnectionBlockedError | undefined;
    readonly beforePrepareRoute?: (
      target: ConnectionTarget,
    ) => Effect.Effect<ConnectionBlockedError | undefined>;
    readonly initialDisabled?: ReadonlyArray<EnvironmentId>;
    readonly directEndpoints?: ServerConfig["directEndpoints"];
  },
) {
  const storedTargets = yield* Ref.make<ReadonlyArray<ConnectionTarget>>(initialTargets);
  const shellCache = yield* Ref.make(new Map([[TARGET.environmentId, CACHED_SNAPSHOT]]));
  const cacheClears = yield* Ref.make<ReadonlyArray<EnvironmentId>>([]);
  const ownedDataClears = yield* Ref.make<ReadonlyArray<EnvironmentId>>([]);
  const sessions = yield* Ref.make<ReadonlyArray<SessionControl>>([]);
  const releasedSessions = yield* Ref.make(0);
  const storedProfiles = yield* Ref.make(
    new Map(initialProfiles.map((profile) => [profile.connectionId, profile])),
  );
  const profileReadCount = yield* Ref.make(0);

  const storedDisabled = yield* Ref.make<ReadonlySet<EnvironmentId>>(
    new Set(options?.initialDisabled ?? []),
  );
  const targetStore = Persistence.ConnectionTargetStore.of({
    list: Ref.get(storedTargets),
    listDisabled: Ref.get(storedDisabled).pipe(Effect.map((ids) => [...ids])),
  });
  const registrationStore = Persistence.ConnectionRegistrationStore.of({
    register: (registration, routes) =>
      Effect.gen(function* () {
        yield* options?.beforeRegistrationRegister?.(registration) ?? Effect.void;
        yield* Ref.update(storedTargets, (current) =>
          replaceRoutes(current, registration.target.environmentId, routes),
        );
        yield* Ref.update(storedProfiles, (current) => {
          const next = new Map(current);
          next.set(registration.profile.connectionId, registration.profile);
          return next;
        });
      }),
    setRoutes: (environmentId, routes) =>
      Ref.update(storedTargets, (current) => replaceRoutes(current, environmentId, routes)),
    remove: (environmentId) =>
      Effect.gen(function* () {
        yield* options?.beforeRegistrationRemove?.(environmentId) ?? Effect.void;
        const removed = (yield* Ref.get(storedTargets)).filter(
          (target) => target.environmentId === environmentId,
        );
        yield* Ref.update(storedTargets, (current) =>
          current.filter((target) => target.environmentId !== environmentId),
        );
        for (const target of removed) {
          if (target._tag === "BearerConnectionTarget") {
            yield* Ref.update(storedProfiles, (current) => {
              const next = new Map(current);
              next.delete(target.connectionId);
              return next;
            });
          }
        }
      }),
    setEnabled: (environmentId, enabled) =>
      Ref.update(storedDisabled, (current) => {
        const next = new Set(current);
        if (enabled) {
          next.delete(environmentId);
        } else {
          next.add(environmentId);
        }
        return next;
      }),
  });
  const cacheStore = Persistence.EnvironmentCacheStore.of({
    loadShell: (environmentId) =>
      Ref.get(shellCache).pipe(
        Effect.map((cache) => Option.fromUndefinedOr(cache.get(environmentId))),
      ),
    saveShell: (environmentId, snapshot) =>
      Ref.update(shellCache, (current) => {
        const next = new Map(current);
        next.set(environmentId, snapshot);
        return next;
      }),
    loadThread: (_environmentId, _threadId) => Effect.succeedNone,
    saveThread: (_environmentId, _thread) => Effect.void,
    removeThread: (_environmentId, _threadId) => Effect.void,
    loadServerConfig: () => Effect.succeedNone,
    saveServerConfig: () => Effect.void,
    clear: (environmentId) =>
      Ref.update(shellCache, (current) => {
        const next = new Map(current);
        next.delete(environmentId);
        return next;
      }).pipe(
        Effect.andThen(
          Ref.update(cacheClears, (environmentIds) => [...environmentIds, environmentId]),
        ),
      ),
  });
  const ownedDataCleanup = Persistence.EnvironmentOwnedDataCleanup.of({
    clear: (environmentId) =>
      Ref.update(ownedDataClears, (environmentIds) => [...environmentIds, environmentId]),
  });
  const networkStatus = yield* SubscriptionRef.make<"unknown" | "offline" | "online">("online");
  const connectivity = Connectivity.Connectivity.of({
    status: SubscriptionRef.get(networkStatus),
    changes: SubscriptionRef.changes(networkStatus),
  });
  const profileStore = ConnectionProfileStore.ConnectionProfileStore.of({
    get: (connectionId) =>
      Ref.update(profileReadCount, (count) => count + 1).pipe(
        Effect.andThen(Ref.get(storedProfiles)),
        Effect.map((current) => Option.fromUndefinedOr(current.get(connectionId))),
      ),
    put: (profile) =>
      Ref.update(storedProfiles, (current) => {
        const next = new Map(current);
        next.set(profile.connectionId, profile);
        return next;
      }),
    remove: (connectionId) =>
      Ref.update(storedProfiles, (current) => {
        const next = new Map(current);
        next.delete(connectionId);
        return next;
      }),
  });
  const connectedRoutes = yield* Ref.make<ReadonlyArray<string>>([]);
  const checkRoute = (route: ConnectionRoute) =>
    Effect.succeed(options?.checkRoute?.(route) ?? "unchecked");
  const connectRoute = (_entry: ConnectionCatalogEntry, route: ConnectionRoute) =>
    Effect.gen(function* () {
      const target = route.target;
      const prepared: PreparedConnection = {
        ...PREPARED,
        environmentId: target.environmentId,
        label: target.label,
        target,
      };
      if (options?.prepareError) return yield* options.prepareError;
      const heldError = options?.beforePrepareRoute
        ? yield* options.beforePrepareRoute(target)
        : undefined;
      if (heldError !== undefined) return yield* heldError;
      const routeError = options?.prepareRoute?.(target);
      if (routeError !== undefined) return yield* routeError;
      yield* Ref.update(connectedRoutes, (current) => [...current, connectionRouteId(target)]);
      yield* options?.beforeSessionConnect?.(target.environmentId) ?? Effect.void;
      const closed = yield* Deferred.make<never, ConnectionTransientError>();
      yield* Ref.update(sessions, (current) => [...current, { closed }]);
      const session = yield* Effect.acquireRelease(
        Effect.succeed({
          client: {} as RpcSession.RpcSession["client"],
          initialConfig:
            options?.directEndpoints === undefined
              ? Effect.die(new Error("Config is not used by registry tests."))
              : Effect.succeed({ directEndpoints: options.directEndpoints } as ServerConfig),
          subscribeServerConfig: () =>
            Stream.die(new Error("Config is not used by registry tests.")),
          ready: Effect.void,
          probe: Effect.void,
          inboundFrames: Effect.succeed(0),
          closed: Deferred.await(closed),
        } satisfies RpcSession.RpcSession),
        () => Ref.update(releasedSessions, (count) => count + 1),
      );
      yield* session.ready;
      return { prepared, session };
    });
  const driver = ConnectionDriver.ConnectionDriver.of({
    connect: (entry, reportProgress) =>
      reportProgress({ stage: "preparing" }).pipe(
        Effect.andThen(
          ConnectionDriver.connectOverRoutes(entry, checkRoute, (route) =>
            connectRoute(entry, route),
          ),
        ),
      ),
    preflight: (_entry, route) =>
      checkRoute(route).pipe(
        Effect.map((check): ConnectionDriver.RoutePreflight =>
          check === "answered" ? "ready" : check,
        ),
      ),
    checkHost: () => Effect.succeed<ConnectionDriver.RouteCheck>("unchecked"),
  });

  const layerCache = Layer.succeed(Persistence.EnvironmentCacheStore, cacheStore);
  const layer = EnvironmentRegistry.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.succeed(Persistence.ConnectionTargetStore, targetStore),
        Layer.succeed(Persistence.ConnectionRegistrationStore, registrationStore),
        Layer.succeed(ConnectionProfileStore.ConnectionProfileStore, profileStore),
        Layer.succeed(Connectivity.Connectivity, connectivity),
        Layer.succeed(
          ConnectionWakeups.ConnectionWakeups,
          ConnectionWakeups.ConnectionWakeups.of({ changes: Stream.never }),
        ),
        Layer.succeed(ConnectionDriver.ConnectionDriver, driver),
        layerCache,
        Layer.succeed(Persistence.EnvironmentOwnedDataCleanup, ownedDataCleanup),
      ),
    ),
  );

  return {
    layer,
    storedTargets,
    shellCache,
    cacheClears,
    ownedDataClears,
    sessions,
    releasedSessions,
    storedProfiles,
    profileReadCount,
    storedDisabled,
    networkStatus,
    connectedRoutes,
  };
});

function routesOf(targets: ReadonlyArray<ConnectionTarget>, environmentId: EnvironmentId) {
  return targets.filter((target) => target.environmentId === environmentId);
}

function hasRoutes(targets: ReadonlyArray<ConnectionTarget>, environmentId: EnvironmentId) {
  return routesOf(targets, environmentId).length > 0;
}

function replaceRoutes(
  current: ReadonlyArray<ConnectionTarget>,
  environmentId: EnvironmentId,
  routes: ReadonlyArray<ConnectionTarget>,
): ReadonlyArray<ConnectionTarget> {
  const firstIndex = current.findIndex((target) => target.environmentId === environmentId);
  const others = current.filter((target) => target.environmentId !== environmentId);
  const insertAt =
    firstIndex === -1
      ? others.length
      : current.slice(0, firstIndex).filter((target) => target.environmentId !== environmentId)
          .length;
  return [...others.slice(0, insertAt), ...routes, ...others.slice(insertAt)];
}

function registrySpanNames<A, E, R>(effect: Effect.Effect<A, E, R>) {
  const names: Array<string> = [];
  const tracer = Tracer.make({
    span: (options) => {
      if (options.name.startsWith("EnvironmentRegistry.")) names.push(options.name);
      return new Tracer.NativeSpan(options);
    },
  });
  return effect.pipe(
    Effect.withTracer(tracer),
    Effect.map(() => names),
  );
}

function awaitConnectionState(
  registry: EnvironmentRegistry.EnvironmentRegistry["Service"],
  environmentId: EnvironmentId,
  predicate: (state: SupervisorConnectionState) => boolean,
) {
  return Effect.gen(function* () {
    const current = yield* registry.state(environmentId);
    if (predicate(current)) {
      return current;
    }
    return yield* registry
      .stateChanges(environmentId)
      .pipe(Stream.filter(predicate), Stream.runHead, Effect.map(Option.getOrThrow));
  });
}

describe("EnvironmentRegistry", () => {
  it.effect("replays connected state when arming a desktop commit observer", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const commits = yield* Ref.make(0);
        const result = yield* runDesktopCommitWithReconnectObserver(
          registry.stateChanges(TARGET.environmentId),
          Ref.update(commits, (count) => count + 1).pipe(
            Effect.andThen(Effect.fail("commit refused")),
          ),
        ).pipe(Effect.flip, Effect.timeout("1 second"));

        expect(result).toBe("commit refused");
        expect(yield* Ref.get(commits)).toBe(1);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not acquire a session after the registry scope has already closed", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      const registryScope = yield* Scope.make();
      const context = yield* Layer.build(harness.layer).pipe(Scope.provide(registryScope));
      const registry = Context.get(context, EnvironmentRegistry.EnvironmentRegistry);
      const dispatcher = new Scheduler.MixedScheduler("sync", () => () => {}).makeDispatcher();
      const scheduler: Scheduler.Scheduler = {
        executionMode: "sync",
        shouldYield: () => false,
        makeDispatcher: () => dispatcher,
      };

      yield* Scope.close(registryScope, Exit.void);
      yield* registry.start.pipe(Effect.provideService(Scheduler.Scheduler, scheduler));
      dispatcher.flush();

      expect(yield* Ref.get(harness.sessions)).toHaveLength(0);
      expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
    }),
  );

  it.effect("starts persisted environments independently", () =>
    Effect.gen(function* () {
      const bothLoadsStarted = yield* Deferred.make<void>();
      const releaseLoads = yield* Deferred.make<void>();
      const loadCount = yield* Ref.make(0);
      const harness = yield* makeHarness([TARGET, SECOND_TARGET], [], {
        beforeSessionConnect: () =>
          Ref.updateAndGet(loadCount, (count) => count + 1).pipe(
            Effect.tap((count) =>
              count === 2 ? Deferred.succeed(bothLoadsStarted, undefined) : Effect.void,
            ),
            Effect.andThen(Deferred.await(releaseLoads)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const start = yield* Effect.forkChild(registry.start);

        yield* Deferred.await(bothLoadsStarted).pipe(Effect.timeout("1 second"));
        yield* Deferred.succeed(releaseLoads, undefined);
        yield* Fiber.join(start);

        expect(yield* Ref.get(loadCount)).toBe(2);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not expose a persisted environment as disconnected while it starts", () =>
    Effect.gen(function* () {
      const connectionStarted = yield* Deferred.make<void>();
      const finishConnection = yield* Deferred.make<void>();
      const harness = yield* makeHarness([TARGET], [], {
        beforeSessionConnect: () =>
          Deferred.succeed(connectionStarted, undefined).pipe(
            Effect.andThen(Deferred.await(finishConnection)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        expect((yield* registry.state(TARGET.environmentId)).phase).toBe("connecting");

        yield* Deferred.await(connectionStarted);
        expect((yield* registry.state(TARGET.environmentId)).phase).toBe("connecting");
        yield* Deferred.succeed(finishConnection, undefined);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("preserves cached data on connection failure and clears it on explicit removal", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        const controls = yield* Ref.get(harness.sessions);
        expect(controls).toHaveLength(1);
        const active = controls[0];
        expect(active).toBeDefined();
        expect((yield* Ref.get(harness.shellCache)).get(TARGET.environmentId)).toEqual(
          CACHED_SNAPSHOT,
        );

        const retryFiber = yield* Effect.forkChild(
          awaitConnectionState(
            registry,
            TARGET.environmentId,
            (state) => state.phase === "backoff",
          ),
        );
        yield* Effect.yieldNow;
        yield* Deferred.fail(
          active!.closed,
          new ConnectionTransientError({
            reason: "transport",
            detail: "Disconnected.",
          }),
        );
        yield* Fiber.join(retryFiber);
        expect((yield* Ref.get(harness.shellCache)).get(TARGET.environmentId)).toEqual(
          CACHED_SNAPSHOT,
        );

        yield* registry.remove(TARGET.environmentId);
        expect(hasRoutes(yield* Ref.get(harness.storedTargets), TARGET.environmentId)).toBe(false);
        expect((yield* Ref.get(harness.shellCache)).has(TARGET.environmentId)).toBe(false);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([TARGET.environmentId]);
        expect((yield* SubscriptionRef.get(registry.entries)).has(TARGET.environmentId)).toBe(
          false,
        );
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect.each([false, true])(
    "a switched-off environment stays off after compatibility clears (learned route: %s)",
    (learned) =>
      Effect.gen(function* () {
        const environmentId = BEARER_TARGET.environmentId;
        const primary = new PrimaryConnectionTarget({ ...TARGET, environmentId });
        const learnedRoute = new BearerConnectionTarget({
          ...BEARER_TARGET,
          connectionId: `learned:${environmentId}:http://192.168.1.20:3773`,
        });
        const learnedProfile = new BearerConnectionProfile({
          ...BEARER_PROFILE,
          connectionId: learnedRoute.connectionId,
          httpBaseUrl: "http://192.168.1.20:3773/",
          wsBaseUrl: "ws://192.168.1.20:3773/",
          learned: true,
        });
        const harness = yield* makeHarness(
          learned ? [primary, learnedRoute] : [primary],
          learned ? [learnedProfile] : [],
          { initialDisabled: [environmentId] },
        );
        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          yield* registry.start;
          const error = new ConnectionBlockedError({
            reason: "unsupported",
            detail: "Use a compatible client.",
          });
          yield* registry.setCompatibility(environmentId, error);
          const entry = (yield* SubscriptionRef.get(registry.entries)).get(environmentId);
          expect(entry).toMatchObject({ enabled: false, unsupportedReason: error.message });
          expect(yield* Effect.flip(registry.setEnabled(environmentId, true))).toMatchObject({
            reason: "unsupported",
          });
          expect(yield* Ref.get(harness.sessions)).toHaveLength(0);
          yield* registry.setCompatibility(environmentId, null);
          expect((yield* SubscriptionRef.get(registry.entries)).get(environmentId)?.enabled).toBe(
            false,
          );
          expect(yield* registry.state(environmentId)).toMatchObject({ desired: false });
          expect(yield* Ref.get(harness.storedDisabled)).toEqual(new Set([environmentId]));
          expect(yield* Ref.get(harness.sessions)).toHaveLength(0);
          yield* registry.setEnabled(environmentId, true);
          yield* awaitConnectionState(
            registry,
            environmentId,
            (state) => state.phase === "connected",
          );
        }).pipe(Effect.provide(harness.layer));
      }),
  );

  it.effect("protocol rejection preserves enablement through recovery and relaunch", () =>
    Effect.gen(function* () {
      const error = new ConnectionBlockedError({
        reason: "unsupported",
        detail: "Use a compatible client.",
      });
      const rejection = yield* Ref.make<ConnectionBlockedError | undefined>(error);
      const harness = yield* makeHarness([HOST_TARGET], [HOST_PROFILE], {
        beforePrepareRoute: () => Ref.get(rejection),
      });
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* SubscriptionRef.changes(registry.entries).pipe(
          Stream.filter(
            (entries) => entries.get(HOST_TARGET.environmentId)?.unsupportedReason !== undefined,
          ),
          Stream.take(1),
          Stream.runDrain,
        );
        expect(yield* Ref.get(harness.storedDisabled)).toEqual(new Set());
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(HOST_TARGET.environmentId),
        ).toMatchObject({ enabled: true, unsupportedReason: error.message });
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => !state.desired && state.phase === "available",
        );
        expect(yield* Ref.get(harness.sessions)).toHaveLength(0);

        yield* Ref.set(rejection, undefined);
        yield* registry.setCompatibility(HOST_TARGET.environmentId, null);
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        yield* registry.setCompatibility(HOST_TARGET.environmentId, error);
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => !state.desired && state.phase === "available",
        );
        expect(yield* Ref.get(harness.releasedSessions)).toBe(1);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const entry = (yield* SubscriptionRef.get(registry.entries)).get(
          HOST_TARGET.environmentId,
        );
        expect(entry?.enabled).toBe(true);
        expect(entry?.unsupportedReason).toBeUndefined();
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect.each([
    { outcome: "connects after a server update", compatible: true, alternateFirst: false },
    { outcome: "connects with a preferred alternate route", compatible: true, alternateFirst: true },
    { outcome: "blocks again if still incompatible", compatible: false, alternateFirst: false },
  ])("retrying an unsupported saved URL $outcome", ({ compatible, alternateFirst }) =>
    Effect.gen(function* () {
      const environmentId = BEARER_TARGET.environmentId;
      const error = new ConnectionBlockedError({
        reason: "unsupported",
        detail: "Update the server to use this client.",
        serverUpdateRequired: true,
      });
      const alternate = new BearerConnectionTarget({
        ...BEARER_TARGET,
        label: "Alternate route",
        connectionId: "bearer-alternate",
      });
      const alternateProfile = new BearerConnectionProfile({
        ...BEARER_PROFILE,
        connectionId: alternate.connectionId,
        label: alternate.label,
        httpBaseUrl: "https://alternate.example.test",
        wsBaseUrl: "wss://alternate.example.test",
      });
      const attempts = yield* Ref.make(0);
      const retryStarted = yield* Deferred.make<void>();
      const retryResult = yield* Deferred.make<ConnectionBlockedError | undefined>();
      const harness = yield* makeHarness(
        alternateFirst ? [alternate, BEARER_TARGET] : [BEARER_TARGET],
        [BEARER_PROFILE, alternateProfile],
        {
          beforePrepareRoute: () =>
            Effect.gen(function* () {
              if ((yield* Ref.updateAndGet(attempts, (count) => count + 1)) === 1) return error;
              yield* Deferred.succeed(retryStarted, undefined);
              return yield* Deferred.await(retryResult);
            }),
        },
      );
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        const awaitUnsupported = SubscriptionRef.changes(registry.entries).pipe(
          Stream.filter((entries) => entries.get(environmentId)?.unsupportedReason !== undefined),
          Stream.take(1),
          Stream.runDrain,
        );
        yield* awaitUnsupported;
        expect((yield* Ref.get(harness.storedDisabled)).has(environmentId)).toBe(false);
        expect((yield* SubscriptionRef.get(registry.entries)).get(environmentId)).toMatchObject({
          target: alternateFirst ? alternate : BEARER_TARGET,
          enabled: true,
          unsupportedReason: error.message,
          serverUpdateRequired: true,
        });
        expect(yield* Ref.get(attempts)).toBe(1);

        yield* registry.setEnabled(environmentId, true);
        yield* Deferred.await(retryStarted);
        const retrying = (yield* SubscriptionRef.get(registry.entries)).get(environmentId);
        expect(retrying?.enabled).toBe(true);
        expect(retrying?.unsupportedReason).toBeUndefined();
        expect(retrying?.serverUpdateRequired).toBeUndefined();
        expect((yield* Ref.get(harness.storedDisabled)).has(environmentId)).toBe(false);

        yield* Deferred.succeed(retryResult, compatible ? undefined : error);
        if (compatible) {
          yield* awaitConnectionState(
            registry,
            environmentId,
            (state) => state.phase === "connected",
          );
          expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        } else {
          yield* awaitUnsupported;
          expect((yield* SubscriptionRef.get(registry.entries)).get(environmentId)).toMatchObject({
            enabled: true,
            unsupportedReason: error.message,
            serverUpdateRequired: true,
          });
          expect((yield* Ref.get(harness.storedDisabled)).has(environmentId)).toBe(false);
          expect(yield* Ref.get(harness.sessions)).toHaveLength(0);
        }
        expect(yield* Ref.get(attempts)).toBe(2);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("switching an environment off disconnects it and persists the flag", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([HOST_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        yield* registry.setEnabled(HOST_TARGET.environmentId, false);
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "available",
        );

        const entry = (yield* SubscriptionRef.get(registry.entries)).get(
          HOST_TARGET.environmentId,
        );
        expect(entry?.enabled).toBe(false);
        expect((yield* Ref.get(harness.storedDisabled)).has(HOST_TARGET.environmentId)).toBe(true);
        expect(hasRoutes(yield* Ref.get(harness.storedTargets), HOST_TARGET.environmentId)).toBe(
          true,
        );
        expect(yield* Ref.get(harness.releasedSessions)).toBe(1);

        yield* registry.setEnabled(HOST_TARGET.environmentId, true);
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        expect((yield* Ref.get(harness.storedDisabled)).has(HOST_TARGET.environmentId)).toBe(
          false,
        );
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("re-registering a switched-off environment keeps it off", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([HOST_TARGET], [HOST_PROFILE], {
        initialDisabled: [HOST_TARGET.environmentId],
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* registry.register(
          new BearerConnectionRegistration({
            target: new BearerConnectionTarget({ ...HOST_TARGET, label: "Renamed" }),
            profile: new BearerConnectionProfile({ ...HOST_PROFILE, label: "Renamed" }),
          }),
        );
        yield* Effect.yieldNow;

        const entry = (yield* SubscriptionRef.get(registry.entries)).get(
          HOST_TARGET.environmentId,
        );
        expect(entry?.target.label).toBe("Renamed");
        expect(entry?.enabled).toBe(false);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(0);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("moves durable streams to a replacement supervisor", () =>
    Effect.gen(function* () {
      const replacement = new BearerConnectionTarget({
        ...HOST_TARGET,
        label: "Replacement host environment",
      });
      const harness = yield* makeHarness([HOST_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const firstObserved = yield* Deferred.make<void>();
        const secondObserved = yield* Deferred.make<void>();
        const labels = yield* Ref.make<ReadonlyArray<string>>([]);
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const subscription = yield* Effect.forkChild(
          registry
            .followStream(
              HOST_TARGET.environmentId,
              Stream.unwrap(
                EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                  Effect.map((supervisor) =>
                    Stream.concat(Stream.succeed(supervisor.target.label), Stream.never),
                  ),
                ),
              ),
            )
            .pipe(
              Stream.tap((label) =>
                Ref.updateAndGet(labels, (current) => [...current, label]).pipe(
                  Effect.flatMap((current) =>
                    current.length === 1
                      ? Deferred.succeed(firstObserved, undefined)
                      : Deferred.succeed(secondObserved, undefined),
                  ),
                ),
              ),
              Stream.runDrain,
            ),
        );

        yield* Deferred.await(firstObserved).pipe(Effect.timeout("1 second"));
        yield* registry.register(
          new BearerConnectionRegistration({
            target: replacement,
            profile: new BearerConnectionProfile({ ...HOST_PROFILE, label: replacement.label }),
          }),
        );
        yield* Deferred.await(secondObserved).pipe(Effect.timeout("1 second"));
        yield* Fiber.interrupt(subscription);

        expect(yield* Ref.get(labels)).toEqual([HOST_TARGET.label, replacement.label]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("keeps the runtime registered when durable removal fails", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([HOST_TARGET], [HOST_PROFILE], {
        beforeRegistrationRemove: () =>
          Effect.fail(
            new Persistence.ConnectionPersistenceError({
              operation: "remove-connection",
              message: "Storage is unavailable.",
            }),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          HOST_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const error = yield* Effect.flip(registry.remove(HOST_TARGET.environmentId));

        expect(error._tag).toBe("ConnectionPersistenceError");
        expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
        expect((yield* SubscriptionRef.get(registry.entries)).has(HOST_TARGET.environmentId)).toBe(
          true,
        );
        expect(hasRoutes(yield* Ref.get(harness.storedTargets), HOST_TARGET.environmentId)).toBe(
          true,
        );
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("starts platform environments without persisting or removing them", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.reconcilePlatform([new PrimaryConnectionRegistration({ target: TARGET })]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect(hasRoutes(yield* Ref.get(harness.storedTargets), TARGET.environmentId)).toBe(false);
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);

        const error = yield* Effect.flip(registry.remove(TARGET.environmentId));
        expect(error._tag).toBe("PlatformEnvironmentRemovalError");
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("gives platform registrations precedence over persisted registrations", () =>
    Effect.gen(function* () {
      for (const registration of [
        new PrimaryConnectionRegistration({ target: TARGET }),
        new BearerConnectionRegistration({ target: BEARER_TARGET, profile: BEARER_PROFILE }),
      ]) {
        const target = registration.target;
        const shadowed =
          registration._tag === "BearerConnectionRegistration"
            ? registration
            : new BearerConnectionRegistration({
                target: new BearerConnectionTarget({
                  environmentId: target.environmentId,
                  label: "Shadowed host environment",
                  connectionId: "shadowed-connection",
                }),
                profile: new BearerConnectionProfile({
                  connectionId: "shadowed-connection",
                  environmentId: target.environmentId,
                  label: "Shadowed host environment",
                  httpBaseUrl: "https://shadowed.example.test",
                  wsBaseUrl: "wss://shadowed.example.test",
                }),
              });
        let failRemoval = true;
        const harness = yield* makeHarness([shadowed.target], [shadowed.profile], {
          beforeRegistrationRemove: () =>
            failRemoval
              ? Effect.fail(
                  new Persistence.ConnectionPersistenceError({
                    operation: "remove-connection",
                    message: "Storage unavailable",
                  }),
                )
              : Effect.void,
        });

        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          yield* registry.reconcilePlatform([registration]);
          expect(hasRoutes(yield* Ref.get(harness.storedTargets), target.environmentId)).toBe(true);

          failRemoval = false;
          yield* registry.reconcilePlatform([registration]);

          expect(
            (yield* SubscriptionRef.get(registry.entries)).get(target.environmentId)?.target,
          ).toEqual(target);
          expect(hasRoutes(yield* Ref.get(harness.storedTargets), target.environmentId)).toBe(
            false,
          );
          if (registration._tag === "BearerConnectionRegistration") {
            expect(yield* registrySpanNames(registry.reconcilePlatform([registration]))).toEqual(
              [],
            );
          }

          yield* registry.register(shadowed);

          expect(
            (yield* SubscriptionRef.get(registry.entries)).get(target.environmentId)?.target,
          ).toEqual(target);
          expect(hasRoutes(yield* Ref.get(harness.storedTargets), target.environmentId)).toBe(
            false,
          );
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }
    }),
  );

  it.effect("rechecks platform ownership after waiting for the environment lease", () =>
    Effect.gen(function* () {
      const registrationStarted = yield* Deferred.make<void>();
      const continueRegistration = yield* Deferred.make<void>();
      const shadowed = new BearerConnectionRegistration({
        target: new BearerConnectionTarget({ ...HOST_TARGET, environmentId: TARGET.environmentId }),
        profile: new BearerConnectionProfile({
          ...HOST_PROFILE,
          environmentId: TARGET.environmentId,
        }),
      });
      const harness = yield* makeHarness([], [], {
        beforeRegistrationRegister: () =>
          Deferred.succeed(registrationStarted, undefined).pipe(
            Effect.andThen(Deferred.await(continueRegistration)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const persistedRegistration = yield* registry
          .register(shadowed)
          .pipe(Effect.forkChild({ startImmediately: true }));
        yield* Deferred.await(registrationStarted);

        const platformRegistration = yield* registry
          .reconcilePlatform([new PrimaryConnectionRegistration({ target: TARGET })])
          .pipe(Effect.forkChild({ startImmediately: true }));
        yield* Effect.yieldNow;
        const removal = yield* Effect.flip(registry.remove(TARGET.environmentId)).pipe(
          Effect.forkChild({ startImmediately: true }),
        );

        yield* Deferred.succeed(continueRegistration, undefined);
        yield* Fiber.join(persistedRegistration);
        yield* Fiber.join(platformRegistration);
        const error = yield* Fiber.join(removal);

        expect(error._tag).toBe("PlatformEnvironmentRemovalError");
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not reacquire a runtime while its registration is being removed", () =>
    Effect.gen(function* () {
      const removalStarted = yield* Deferred.make<void>();
      const continueRemoval = yield* Deferred.make<void>();
      const harness = yield* makeHarness([TARGET], [], {
        beforeRegistrationRemove: () =>
          Deferred.succeed(removalStarted, undefined).pipe(
            Effect.andThen(Deferred.await(continueRemoval)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const removal = yield* Effect.forkChild(registry.remove(TARGET.environmentId));
        yield* Deferred.await(removalStarted);

        const stateLookup = yield* Effect.forkChild(
          Effect.flip(registry.state(TARGET.environmentId)),
        );
        yield* Effect.yieldNow;
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);

        yield* Deferred.succeed(continueRemoval, undefined);
        yield* Fiber.join(removal);
        const error = yield* Fiber.join(stateLookup);
        expect(error._tag).toBe("EnvironmentNotRegisteredError");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("platform refreshes preserve unsupported state for the same endpoint", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const registration = new PrimaryConnectionRegistration({ target: TARGET });
        yield* registry.reconcilePlatform([registration]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        const error = new ConnectionBlockedError({
          reason: "unsupported",
          detail: "Use a compatible client.",
        });
        yield* registry.setCompatibility(TARGET.environmentId, error);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "available",
        );
        yield* registry.reconcilePlatform([registration]);
        yield* registry.reconcilePlatform([registration]);
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId),
        ).toMatchObject({ enabled: true, unsupportedReason: error.message });
        yield* registry.reconcilePlatform([
          new PrimaryConnectionRegistration({
            target: new PrimaryConnectionTarget({ ...TARGET, label: "Renamed" }),
          }),
        ]);
        expect(yield* registry.state(TARGET.environmentId)).toMatchObject({
          desired: false,
          phase: "available",
        });
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        yield* registry.reconcilePlatform([
          new PrimaryConnectionRegistration({
            target: new PrimaryConnectionTarget({
              ...TARGET,
              httpBaseUrl: "https://changed.example.test",
            }),
          }),
        ]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)
            ?.unsupportedReason,
        ).toBeUndefined();
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("keeps one session per environment across concurrent registrations and retries", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const registration = new PrimaryConnectionRegistration({ target: TARGET });
        yield* Effect.all(
          Array.from({ length: 5 }, () => registry.reconcilePlatform([registration])),
          { concurrency: "unbounded", discard: true },
        );
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        yield* Effect.all(
          [
            ...Array.from({ length: 5 }, () => registry.reconcilePlatform([registration])),
            ...Array.from({ length: 5 }, () => registry.retryNow(TARGET.environmentId)),
            registry.reconcilePlatform([registration]),
          ],
          { concurrency: "unbounded", discard: true },
        );
        for (let attempt = 0; attempt < 100; attempt += 1) {
          yield* Effect.yieldNow;
        }

        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
        expect(yield* registry.state(TARGET.environmentId)).toMatchObject({
          phase: "connected",
          generation: 1,
        });
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("retains a healthy runtime when the platform repeats an identical registration", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const registration = new PrimaryConnectionRegistration({ target: TARGET });
        yield* registry.reconcilePlatform([registration]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const repeated = yield* registrySpanNames(
          registry.reconcilePlatform([
            new PrimaryConnectionRegistration({ target: new PrimaryConnectionTarget(TARGET) }),
          ]),
        );

        expect(repeated).toEqual([]);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);

        const changedTarget = new PrimaryConnectionTarget({
          ...TARGET,
          httpBaseUrl: "https://changed.example.test",
        });
        yield* registry.reconcilePlatform([
          new PrimaryConnectionRegistration({ target: changedTarget }),
        ]);
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(changedTarget);

        yield* registry.reconcilePlatform([]);
        expect((yield* SubscriptionRef.get(registry.entries)).has(TARGET.environmentId)).toBe(
          false,
        );
        expect(yield* registrySpanNames(registry.reconcilePlatform([]))).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );
});

describe("EnvironmentRegistry routes", () => {
  const LAN_TARGET = new BearerConnectionTarget({
    environmentId: HOST_TARGET.environmentId,
    label: HOST_TARGET.label,
    connectionId: "bearer:lan",
  });
  const LAN_PROFILE = new BearerConnectionProfile({
    connectionId: LAN_TARGET.connectionId,
    environmentId: LAN_TARGET.environmentId,
    label: LAN_TARGET.label,
    httpBaseUrl: "http://192.168.1.10:3773/",
    wsBaseUrl: "ws://192.168.1.10:3773/",
  });
  const lanRegistration = (target = LAN_TARGET, profile = LAN_PROFILE) =>
    new BearerConnectionRegistration({ target, profile });

  it.effect("adds a paired LAN route ahead of an existing route instead of replacing it", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([HOST_TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(lanRegistration());

        expect(routesOf(yield* Ref.get(harness.storedTargets), LAN_TARGET.environmentId)).toEqual([
          LAN_TARGET,
          HOST_TARGET,
        ]);
        const entry = (yield* SubscriptionRef.get(registry.entries)).get(LAN_TARGET.environmentId);
        expect(entry?.target).toEqual(LAN_TARGET);
        expect(entry?.alternateRoutes?.map((route) => route.target)).toEqual([HOST_TARGET]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("re-pairing the same address replaces that route rather than adding another", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([LAN_TARGET, HOST_TARGET], [LAN_PROFILE, HOST_PROFILE]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const repaired = new BearerConnectionTarget({
          ...LAN_TARGET,
          connectionId: "bearer:lan-again",
        });
        yield* registry.register(
          lanRegistration(
            repaired,
            new BearerConnectionProfile({ ...LAN_PROFILE, connectionId: repaired.connectionId }),
          ),
        );

        expect(routesOf(yield* Ref.get(harness.storedTargets), LAN_TARGET.environmentId)).toEqual([
          repaired,
          HOST_TARGET,
        ]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("reorders routes and rejects an order that drops one", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([LAN_TARGET, HOST_TARGET], [LAN_PROFILE, HOST_PROFILE]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.reorderRoutes(LAN_TARGET.environmentId, [
          HOST_TARGET.connectionId,
          LAN_TARGET.connectionId,
        ]);
        expect(routesOf(yield* Ref.get(harness.storedTargets), LAN_TARGET.environmentId)).toEqual([
          HOST_TARGET,
          LAN_TARGET,
        ]);

        const error = yield* registry
          .reorderRoutes(LAN_TARGET.environmentId, [HOST_TARGET.connectionId])
          .pipe(Effect.flip);
        expect(error._tag).toBe("ConnectionBlockedError");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("removing the last route forgets the environment", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([LAN_TARGET, HOST_TARGET], [LAN_PROFILE, HOST_PROFILE]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.removeRoute(LAN_TARGET.environmentId, LAN_TARGET.connectionId);
        expect(routesOf(yield* Ref.get(harness.storedTargets), LAN_TARGET.environmentId)).toEqual([
          HOST_TARGET,
        ]);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);

        yield* registry.removeRoute(LAN_TARGET.environmentId, HOST_TARGET.connectionId);
        expect(hasRoutes(yield* Ref.get(harness.storedTargets), LAN_TARGET.environmentId)).toBe(
          false,
        );
        expect(yield* Ref.get(harness.cacheClears)).toEqual([LAN_TARGET.environmentId]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("loads saved routes grouped by environment, in saved order", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness(
        [LAN_TARGET, BEARER_TARGET, HOST_TARGET],
        [LAN_PROFILE, BEARER_PROFILE, HOST_PROFILE],
      );
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const entries = yield* SubscriptionRef.get(registry.entries);
        expect([...entries.keys()]).toEqual([
          LAN_TARGET.environmentId,
          BEARER_TARGET.environmentId,
        ]);
        expect(entries.get(LAN_TARGET.environmentId)?.alternateRoutes?.[0]?.target).toEqual(
          HOST_TARGET,
        );
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("saves the Tailscale mark when the server reports a learned address as tailnet", () =>
    Effect.gen(function* () {
      const httpBaseUrl = "http://100.101.102.103:3773/";
      const learnedId = `learned:${LAN_TARGET.environmentId}:http://100.101.102.103:3773`;
      const learned = new BearerConnectionTarget({ ...LAN_TARGET, connectionId: learnedId });
      const learnedProfile = new BearerConnectionProfile({
        connectionId: learnedId,
        environmentId: LAN_TARGET.environmentId,
        label: LAN_TARGET.label,
        httpBaseUrl,
        wsBaseUrl: "ws://100.101.102.103:3773/",
        learned: true,
      });
      const harness = yield* makeHarness([LAN_TARGET, learned], [LAN_PROFILE, learnedProfile], {
        directEndpoints: [
          { kind: "lan", httpBaseUrl: LAN_PROFILE.httpBaseUrl },
          { kind: "tailnet", httpBaseUrl },
        ],
      });
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* SubscriptionRef.changes(registry.entries).pipe(
          Stream.map((entries) => entries.get(LAN_TARGET.environmentId)?.alternateRoutes?.[0]),
          Stream.filter((route) => route !== undefined && connectionRouteKind(route) === "tailnet"),
          Stream.runHead,
        );
        expect((yield* Ref.get(harness.storedProfiles)).get(learnedId)).toMatchObject({
          network: "tailscale",
          learned: true,
          httpBaseUrl,
        });
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("drops a learned route whose profile was not saved instead of duplicating it", () =>
    Effect.gen(function* () {
      const learnedId = `learned:${LAN_TARGET.environmentId}:http://100.64.0.9:3773`;
      const learned = new BearerConnectionTarget({ ...LAN_TARGET, connectionId: learnedId });
      const harness = yield* makeHarness([LAN_TARGET, learned, learned], [LAN_PROFILE]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const entry = (yield* SubscriptionRef.get(registry.entries)).get(LAN_TARGET.environmentId);
        expect(entry?.target).toEqual(LAN_TARGET);
        expect(entry?.alternateRoutes ?? []).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );
});
