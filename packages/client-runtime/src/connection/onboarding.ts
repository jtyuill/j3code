import type { EnvironmentId } from "@t3tools/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as HttpClient from "effect/http/HttpClient";

import { deriveWsBaseUrl, normalizeHttpBaseUrl } from "../environment/endpoint.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";
import {
  BearerConnectionProfile,
  BearerConnectionRegistration,
  type ConnectionCatalogEntry,
} from "./catalog.ts";
import { mapRemoteEnvironmentError } from "./errors.ts";
import { BearerConnectionTarget, ConnectionBlockedError, type ConnectionAttemptError } from "./model.ts";
import * as Persistence from "../platform/persistence.ts";
import * as EnvironmentRegistry from "./registry.ts";
import { orchestrationProtocolCompatibilityError } from "./compatibility.ts";
import { connectionRoutes, routeEntry } from "./routes.ts";

export interface DirectConnectionInput {
  /** `http://shieldy:3780`, `shieldy:3780`, or any URL on that origin. */
  readonly host: string;
}

export interface HostConnectionUpdateInput {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly httpBaseUrl: string;
}

export class ConnectionOnboarding extends Context.Service<
  ConnectionOnboarding,
  {
    /** Saves a host that takes no credential (j3 over the tailnet) and connects to it. */
    readonly registerDirect: (
      input: DirectConnectionInput,
    ) => Effect.Effect<
      EnvironmentId,
      ConnectionAttemptError | Persistence.ConnectionPersistenceError
    >;
    /** Renames a saved host or moves it to another URL. */
    readonly updateHost: (
      input: HostConnectionUpdateInput,
    ) => Effect.Effect<void, ConnectionAttemptError | Persistence.ConnectionPersistenceError>;
  }
>()("@t3tools/client-runtime/connection/onboarding/ConnectionOnboarding") {}

function bearerConnectionId(environmentId: EnvironmentId, httpBaseUrl: string): string {
  return `bearer:${environmentId}:${new URL(httpBaseUrl).origin}`;
}

const normalizeHost = (host: string) =>
  Effect.try({
    try: () => {
      const trimmed = host.trim();
      if (trimmed === "") throw new Error("Enter a host URL.");
      return normalizeHttpBaseUrl(/^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`);
    },
    catch: (cause) =>
      new ConnectionBlockedError({
        reason: "configuration",
        detail: cause instanceof Error ? cause.message : "The host URL is invalid.",
      }),
  });

export const prepareDirectRegistration = Effect.fn(
  "clientRuntime.connection.onboarding.prepareDirectRegistration",
)(function* (input: DirectConnectionInput) {
  const httpBaseUrl = yield* normalizeHost(input.host);
  const descriptor = yield* fetchRemoteEnvironmentDescriptor({ httpBaseUrl }).pipe(
    Effect.mapError(mapRemoteEnvironmentError),
  );
  const compatibilityError = orchestrationProtocolCompatibilityError(descriptor);
  if (compatibilityError !== null) {
    return yield* compatibilityError;
  }
  const connectionId = bearerConnectionId(descriptor.environmentId, httpBaseUrl);
  return new BearerConnectionRegistration({
    target: new BearerConnectionTarget({
      environmentId: descriptor.environmentId,
      label: descriptor.label,
      connectionId,
    }),
    profile: new BearerConnectionProfile({
      connectionId,
      environmentId: descriptor.environmentId,
      label: descriptor.label,
      httpBaseUrl,
      wsBaseUrl: deriveWsBaseUrl(httpBaseUrl),
    }),
  });
});

const registerDirectConnection = Effect.fn(
  "clientRuntime.connection.onboarding.registerDirectConnection",
)(function* (input: DirectConnectionInput) {
  const registration = yield* prepareDirectRegistration(input);
  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  if (!(yield* registry.register(registration))) {
    return registration.target.environmentId;
  }
  yield* registry.setCompatibility(registration.target.environmentId, null);
  yield* registry
    .setEnabled(registration.target.environmentId, true)
    .pipe(Effect.catchTags({ EnvironmentNotRegisteredError: () => Effect.void }));
  return registration.target.environmentId;
});

const isBearerProfile = Schema.is(BearerConnectionProfile);

const updateHostConnection = Effect.fn("clientRuntime.connection.onboarding.updateHostConnection")(
  function* (input: HostConnectionUpdateInput) {
    const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
    const saved = (yield* SubscriptionRef.get(registry.entries)).get(input.environmentId);
    const route =
      saved === undefined
        ? undefined
        : connectionRoutes(saved).find(
            (candidate) => candidate.target._tag === "BearerConnectionTarget",
          );
    const entry = saved === undefined || route === undefined ? saved : routeEntry(saved, route);
    const registration = yield* prepareHostConnectionUpdate({
      input,
      entry: Option.fromUndefinedOr(entry),
    });
    yield* registry.register(registration);
  },
);

export const prepareHostConnectionUpdate = Effect.fn(
  "clientRuntime.connection.onboarding.prepareHostConnectionUpdate",
)(function* (options: {
  readonly input: HostConnectionUpdateInput;
  readonly entry: Option.Option<ConnectionCatalogEntry>;
}) {
  const entry = Option.getOrNull(options.entry);
  if (
    entry === null ||
    entry.target._tag !== "BearerConnectionTarget" ||
    Option.isNone(entry.profile) ||
    !isBearerProfile(entry.profile.value)
  ) {
    return yield* new ConnectionBlockedError({
      reason: "configuration",
      detail: "Only saved environments can be edited.",
    });
  }

  const label = options.input.label.trim();
  if (label === "") {
    return yield* new ConnectionBlockedError({
      reason: "configuration",
      detail: "Environment label cannot be empty.",
    });
  }
  const httpBaseUrl = yield* Effect.try({
    try: () => normalizeHttpBaseUrl(options.input.httpBaseUrl),
    catch: (cause) =>
      new ConnectionBlockedError({
        reason: "configuration",
        detail: cause instanceof Error ? cause.message : "The environment URL is invalid.",
      }),
  });
  const connectionId = entry.target.connectionId;
  return new BearerConnectionRegistration({
    target: new BearerConnectionTarget({
      environmentId: options.input.environmentId,
      label,
      connectionId,
    }),
    profile: new BearerConnectionProfile({
      connectionId,
      environmentId: options.input.environmentId,
      label,
      httpBaseUrl,
      wsBaseUrl: deriveWsBaseUrl(httpBaseUrl),
    }),
  });
});

/** @public */
export const make = Effect.gen(function* () {
  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  const httpClient = yield* HttpClient.HttpClient;

  return ConnectionOnboarding.of({
    registerDirect: Effect.fnUntraced(
      function* (input) {
        return yield* registerDirectConnection(input);
      },
      Effect.provideService(EnvironmentRegistry.EnvironmentRegistry, registry),
      Effect.provideService(HttpClient.HttpClient, httpClient),
    ),
    updateHost: Effect.fnUntraced(
      function* (input) {
        return yield* updateHostConnection(input);
      },
      Effect.provideService(EnvironmentRegistry.EnvironmentRegistry, registry),
    ),
  });
});

export const layer = Layer.effect(ConnectionOnboarding, make);
