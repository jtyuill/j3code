import type { AuthClientPresentationMetadata } from "@t3tools/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as HttpClient from "effect/http/HttpClient";

import * as ClientCapabilities from "../platform/capabilities.ts";
import { BearerConnectionProfile, type ConnectionCatalogEntry } from "./catalog.ts";
import {
  environmentMismatchError,
  mapRemoteEnvironmentError,
  profileMissingError,
} from "./errors.ts";
import type { BearerConnectionTarget, ConnectionTarget, PreparedConnection } from "./model.ts";
import { ConnectionBlockedError, type ConnectionAttemptError } from "./model.ts";
import {
  appendOrchestrationProtocol,
  orchestrationProtocolCompatibilityError,
} from "./compatibility.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";

export class ConnectionResolver extends Context.Service<
  ConnectionResolver,
  {
    readonly prepare: (
      entry: ConnectionCatalogEntry,
    ) => Effect.Effect<PreparedConnection, ConnectionAttemptError>;
  }
>()("@t3tools/client-runtime/connection/resolver/ConnectionResolver") {}

const isBearerProfile = Schema.is(BearerConnectionProfile);

function appendClientConnectionParams(
  url: URL,
  clientMetadata: AuthClientPresentationMetadata | undefined,
): void {
  if (clientMetadata?.surface) {
    url.searchParams.set("clientSurface", clientMetadata.surface);
  }
  if (clientMetadata?.appVersion) {
    url.searchParams.set("clientAppVersion", clientMetadata.appVersion);
  }
  if (clientMetadata?.deviceType) {
    const deviceType =
      clientMetadata.deviceType === "mobile"
        ? "phone"
        : clientMetadata.deviceType === "desktop" || clientMetadata.deviceType === "tablet"
          ? clientMetadata.deviceType
          : "unknown";
    url.searchParams.set("clientDeviceType", deviceType);
  }
  if (clientMetadata?.os) {
    url.searchParams.set("clientOs", clientMetadata.os);
  }
  if (clientMetadata?.surface === "mobile") {
    if (clientMetadata.osMajorVersion !== undefined) {
      url.searchParams.set("clientOsMajorVersion", String(clientMetadata.osMajorVersion));
    }
    if (clientMetadata.deviceModel) {
      url.searchParams.set("clientDeviceModel", clientMetadata.deviceModel);
    }
  }
  url.searchParams.set("connectionMethod", "direct");
}

/** j3 hosts take no credential: the socket URL carries only client metadata. */
function socketUrl(
  wsBaseUrl: string,
  clientMetadata: AuthClientPresentationMetadata | undefined,
): string {
  const url = new URL(wsBaseUrl);
  if (url.pathname === "" || url.pathname === "/") {
    url.pathname = "/ws";
  }
  appendClientConnectionParams(url, clientMetadata);
  return url.toString();
}

const resolveBearerTarget = Effect.fn("clientRuntime.connection.broker.bearer")(function* (
  entry: ConnectionCatalogEntry & { readonly target: BearerConnectionTarget },
  clientMetadata: AuthClientPresentationMetadata,
) {
  const target = entry.target;
  const profile = yield* Effect.fromOption(entry.profile, () =>
    profileMissingError(target.connectionId),
  );
  if (!isBearerProfile(profile)) {
    return yield* new ConnectionBlockedError({
      reason: "configuration",
      detail: `Connection profile ${target.connectionId} is not a host connection.`,
    });
  }
  if (profile.environmentId !== target.environmentId) {
    return yield* environmentMismatchError({
      expected: target.environmentId,
      actual: profile.environmentId,
    });
  }
  return {
    environmentId: target.environmentId,
    label: profile.label,
    httpBaseUrl: profile.httpBaseUrl,
    socketUrl: socketUrl(profile.wsBaseUrl, clientMetadata),
    target,
  } satisfies PreparedConnection;
});

/** @public */
export const make = Effect.gen(function* () {
  const presentation = yield* ClientCapabilities.ClientPresentation;
  const httpClient = yield* HttpClient.HttpClient;

  const resolve = Effect.fn("clientRuntime.connection.broker.resolve")(function* (
    entry: ConnectionCatalogEntry,
  ) {
    const target: ConnectionTarget = entry.target;
    yield* Effect.annotateCurrentSpan({
      "connection.environment.id": target.environmentId,
      "connection.target.kind": target._tag,
    });
    const prepared: PreparedConnection =
      target._tag === "PrimaryConnectionTarget"
        ? {
            environmentId: target.environmentId,
            label: target.label,
            httpBaseUrl: target.httpBaseUrl,
            socketUrl: socketUrl(target.wsBaseUrl, presentation.metadata),
            target,
          }
        : yield* resolveBearerTarget({ ...entry, target }, presentation.metadata);
    const descriptor = yield* fetchRemoteEnvironmentDescriptor({
      httpBaseUrl: prepared.httpBaseUrl,
    }).pipe(
      Effect.mapError(mapRemoteEnvironmentError),
      Effect.provideService(HttpClient.HttpClient, httpClient),
    );
    if (descriptor.environmentId !== target.environmentId) {
      return yield* environmentMismatchError({
        expected: target.environmentId,
        actual: descriptor.environmentId,
      });
    }
    return { prepared, descriptor };
  });

  const prepare = Effect.fn("clientRuntime.connection.broker.prepare")(function* (
    entry: ConnectionCatalogEntry,
  ) {
    const { prepared, descriptor } = yield* resolve(entry);
    const compatibilityError = orchestrationProtocolCompatibilityError(descriptor);
    if (compatibilityError !== null) {
      return yield* compatibilityError;
    }
    return {
      ...prepared,
      socketUrl: appendOrchestrationProtocol(prepared.socketUrl),
    };
  });

  return ConnectionResolver.of({ prepare });
});

export const layer = Layer.effect(ConnectionResolver, make);
