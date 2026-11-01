import { EnvironmentId, ORCHESTRATION_PROTOCOL_VERSION } from "@t3tools/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";

import * as ConnectionResolver from "./resolver.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import {
  BearerConnectionProfile,
  type ConnectionCatalogEntry,
  type ConnectionProfile,
} from "./catalog.ts";
import { BearerConnectionTarget, PrimaryConnectionTarget, type ConnectionTarget } from "./model.ts";
import * as RpcHttp from "../rpc/http.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

function catalogEntry(
  target: ConnectionTarget,
  profile: Option.Option<ConnectionProfile> = Option.none(),
): ConnectionCatalogEntry {
  return { target, profile, enabled: true };
}

function makeResolver(options?: {
  readonly descriptorEnvironmentId?: EnvironmentId;
  readonly descriptorProtocolVersion?: number | null;
}) {
  const requested: string[] = [];
  const fetchFn: typeof fetch = (request) => {
    requested.push(String(request));
    return Promise.resolve(
      Response.json({
        environmentId: options?.descriptorEnvironmentId ?? ENVIRONMENT_ID,
        label: "shieldy",
        platform: { os: "linux", arch: "x64" },
        serverVersion: "0.0.0-test",
        ...(options?.descriptorProtocolVersion === undefined
          ? { orchestrationProtocolVersion: ORCHESTRATION_PROTOCOL_VERSION }
          : options.descriptorProtocolVersion === null
            ? {}
            : { orchestrationProtocolVersion: options.descriptorProtocolVersion }),
        capabilities: { repositoryIdentity: true },
      }),
    );
  };
  const layer = ConnectionResolver.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        RpcHttp.layerRemoteHttpClient(fetchFn),
        Layer.succeed(
          ClientCapabilities.ClientPresentation,
          ClientCapabilities.ClientPresentation.of({
            metadata: { label: "j3 iOS", deviceType: "mobile", surface: "mobile", os: "ios" },
          }),
        ),
      ),
    ),
  );
  return { requested, resolver: ConnectionResolver.ConnectionResolver.pipe(Effect.provide(layer)) };
}

const HOST_TARGET = new BearerConnectionTarget({
  environmentId: ENVIRONMENT_ID,
  label: "shieldy",
  connectionId: "host",
});
const HOST_PROFILE = new BearerConnectionProfile({
  connectionId: "host",
  environmentId: ENVIRONMENT_ID,
  label: "shieldy",
  httpBaseUrl: "http://shieldy:3780/",
  wsBaseUrl: "ws://shieldy:3780/",
});

describe("ConnectionResolver", () => {
  it.effect("connects to a saved host with no credential", () =>
    Effect.gen(function* () {
      const { requested, resolver } = makeResolver();
      const broker = yield* resolver;

      expect(yield* broker.prepare(catalogEntry(HOST_TARGET, Option.some(HOST_PROFILE)))).toEqual({
        environmentId: ENVIRONMENT_ID,
        label: "shieldy",
        httpBaseUrl: "http://shieldy:3780/",
        socketUrl:
          "ws://shieldy:3780/ws?clientSurface=mobile&clientDeviceType=phone&clientOs=ios&connectionMethod=direct&orchestrationProtocol=2",
        target: HOST_TARGET,
      });
      expect(requested).toEqual(["http://shieldy:3780/.well-known/t3/environment"]);
    }),
  );

  it.effect("prepares a primary environment the same way", () =>
    Effect.gen(function* () {
      const broker = yield* makeResolver().resolver;
      const target = new PrimaryConnectionTarget({
        environmentId: ENVIRONMENT_ID,
        label: "Primary",
        httpBaseUrl: "http://127.0.0.1:3780",
        wsBaseUrl: "ws://127.0.0.1:3780",
      });

      expect((yield* broker.prepare(catalogEntry(target))).socketUrl).toBe(
        "ws://127.0.0.1:3780/ws?clientSurface=mobile&clientDeviceType=phone&clientOs=ios&connectionMethod=direct&orchestrationProtocol=2",
      );
    }),
  );

  it.effect("blocks a host without a saved profile", () =>
    Effect.gen(function* () {
      const broker = yield* makeResolver().resolver;
      const error = yield* Effect.flip(broker.prepare(catalogEntry(HOST_TARGET)));

      expect(error).toMatchObject({ _tag: "ConnectionBlockedError", reason: "configuration" });
    }),
  );

  it.effect("blocks a host that answers as another environment", () =>
    Effect.gen(function* () {
      const broker = yield* makeResolver({
        descriptorEnvironmentId: EnvironmentId.make("someone-else"),
      }).resolver;
      const error = yield* Effect.flip(
        broker.prepare(catalogEntry(HOST_TARGET, Option.some(HOST_PROFILE))),
      );

      expect(error).toMatchObject({ _tag: "ConnectionBlockedError", reason: "configuration" });
      expect(error.message).toContain("does not match");
    }),
  );

  it.effect("blocks an old host during discovery before opening orchestration RPC", () =>
    Effect.gen(function* () {
      const broker = yield* makeResolver({ descriptorProtocolVersion: null }).resolver;
      const error = yield* Effect.flip(
        broker.prepare(catalogEntry(HOST_TARGET, Option.some(HOST_PROFILE))),
      );

      expect(error).toMatchObject({ reason: "unsupported" });
      expect(error.message).toContain("Update j3-server on shieldy");
    }),
  );
});
