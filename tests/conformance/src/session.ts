import { appendFileSync } from "node:fs";

import {
  ORCHESTRATION_PROTOCOL_VERSION,
  type ServerConfig,
  type ServerConfigStreamEvent,
  WsRpcGroup,
} from "@t3tools/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import * as ManagedRuntime from "effect/ManagedRuntime";
import * as Schedule from "effect/Schedule";
import * as Stream from "effect/Stream";
import * as RpcClient from "effect/rpc/RpcClient";
import * as RpcSerialization from "effect/rpc/RpcSerialization";
import * as Socket from "effect/socket/Socket";

import { StepFailure, formatCause } from "./report.ts";
import { Watcher } from "./watcher.ts";

/** The same client t3's client-runtime builds (`CR/rpc/protocol.ts`). */
const makeWsRpcClient = RpcClient.make(WsRpcGroup);
export type WsClient = Effect.Success<typeof makeWsRpcClient>;

class WsClientService extends Context.Service<WsClientService, WsClient>()(
  "j3-conformance/WsClient",
) {}

/** Mirrors `CR/rpc/session.ts` + `CR/rpc/protocol.ts`. */
const SOCKET_OPEN_TIMEOUT = "15 seconds";
const PING_TIMEOUT = "15 seconds";

/**
 * The WebSocket URL the mobile client dials (`CR/connection/resolver.ts` primarySocketUrl +
 * `appendClientConnectionParams` + `appendOrchestrationProtocol`).
 */
export function mobileSocketUrl(httpBaseUrl: string): string {
  const url = new URL(httpBaseUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.search = "";
  url.hash = "";
  if (url.pathname === "" || url.pathname === "/") url.pathname = "/ws";
  url.searchParams.set("clientSurface", "mobile");
  url.searchParams.set("clientAppVersion", "0.0.0-conformance");
  url.searchParams.set("clientDeviceType", "phone");
  url.searchParams.set("clientOs", "iOS");
  url.searchParams.set("clientOsMajorVersion", "26");
  url.searchParams.set("clientDeviceModel", "j3-conformance");
  url.searchParams.set("connectionMethod", "direct");
  url.searchParams.set("orchestrationProtocol", String(ORCHESTRATION_PROTOCOL_VERSION));
  return url.toString();
}

/** The `subscribeServerConfig` payload the mobile app sends (`M/connection/runtime.ts`). */
const SERVER_CONFIG_INPUT = {
  usageLimitSources: true,
  usageLimitsCommand: true,
  workspaceSnapshotsOnDemand: true,
} as const;

export interface SessionOptions {
  readonly label: string;
  readonly socketUrl: string;
  /** Appends every WebSocket text frame (both directions) as JSONL. */
  readonly framesFile: string | undefined;
}

/**
 * One WebSocket = one RPC session, built the way t3's client-runtime builds it: `Socket`
 * over the global `WebSocket`, `RpcClient.makeProtocolSocket` without retries, JSON
 * serialization, and the contracts' `WsRpcGroup`. Every response and stream item is decoded by
 * the contracts schemas inside effect's RpcClient; a value that fails to decode surfaces as a
 * defect carrying the `SchemaError`.
 */
export class Session {
  readonly connected: Promise<"open" | "closed">;
  readonly config: Watcher<ServerConfigStreamEvent>;
  private readonly runtime: ManagedRuntime.ManagedRuntime<WsClientService, never>;
  private readonly aborts = new Set<AbortController>();
  private closed = false;
  private readonly options: SessionOptions;

  constructor(options: SessionOptions) {
    this.options = options;
    const opened = Promise.withResolvers<"open" | "closed">();
    this.connected = opened.promise;
    const hooks = {
      onConnect: Effect.sync(() => opened.resolve("open")),
      onDisconnect: Effect.sync(() => opened.resolve("closed")),
    };
    const framesFile = options.framesFile;
    const label = options.label;
    const webSocketConstructor = (
      url: string,
      protocols?: Socket.WebSocketConstructorOptions,
    ): globalThis.WebSocket => {
      const socket = new globalThis.WebSocket(url, protocols as string | string[] | undefined);
      if (framesFile !== undefined) {
        const log = (dir: string, data: unknown) =>
          appendFileSync(
            framesFile,
            `${JSON.stringify({ session: label, dir, at: new Date().toISOString(), raw: typeof data === "string" ? data : `<${typeof data}>` })}\n`,
          );
        const send = socket.send.bind(socket);
        socket.send = (data) => {
          log("client->server", data);
          send(data);
        };
        socket.addEventListener("message", (event) => log("server->client", event.data));
      }
      return socket;
    };
    const socketLayer = Socket.layerWebSocket(options.socketUrl, {
      openTimeout: SOCKET_OPEN_TIMEOUT,
    }).pipe(Layer.provide(Layer.succeed(Socket.WebSocketConstructor, webSocketConstructor)));
    const protocolLayer = Layer.effect(
      RpcClient.Protocol,
      RpcClient.makeProtocolSocket({
        pingTimeout: PING_TIMEOUT,
        retryTransientErrors: false,
        retryPolicy: Schedule.recurs(0),
      }),
    ).pipe(
      Layer.provide(
        Layer.mergeAll(
          socketLayer,
          RpcSerialization.layerJson,
          Layer.succeed(RpcClient.ConnectionHooks, hooks),
        ),
      ),
    );
    this.runtime = ManagedRuntime.make(
      Layer.effect(WsClientService, makeWsRpcClient).pipe(Layer.provide(protocolLayer)),
    );
    let snapshots = 0;
    this.config = this.watch(
      "subscribeServerConfig",
      (client) => client.subscribeServerConfig(SERVER_CONFIG_INPUT),
      (event) => {
        if (event.type === "snapshot") snapshots += 1;
      },
      () => `${snapshots} snapshot(s), ${this.config.items.length} item(s)`,
    );
  }

  get label(): string {
    return this.options.label;
  }

  /** The first `snapshot` config: the handshake `session.ready` waits for. */
  firstConfig(timeoutMs: number): Promise<ServerConfig> {
    return this.config.until("the first config snapshot", timeoutMs, () => {
      const first = this.config.items[0];
      if (first === undefined) return undefined;
      if (first.type !== "snapshot") {
        throw new StepFailure(
          `the first subscribeServerConfig item must be {type:"snapshot"}, got type "${first.type}"`,
        );
      }
      return first.config;
    });
  }

  /** Runs one unary call; any failure (typed, defect, decode, transport, timeout) throws. */
  async call<A, E>(
    what: string,
    timeoutMs: number,
    run: (client: WsClient) => Effect.Effect<A, E>,
  ): Promise<A> {
    const exit = await this.runtime.runPromiseExit(
      Effect.flatMap(Effect.service(WsClientService), run).pipe(
        Effect.timeoutOrElse({
          duration: timeoutMs,
          orElse: () => Effect.die(new Error(`no reply within ${timeoutMs / 1000}s`)),
        }),
      ),
    );
    if (Exit.isSuccess(exit)) return exit.value;
    throw new StepFailure(`${what} failed:\n${formatCause(exit.cause)}`);
  }

  /** Subscribes a stream RPC; items feed the returned watcher until `stop()` or session close. */
  watch<A, E>(
    label: string,
    open: (client: WsClient) => Stream.Stream<A, E>,
    onItem: (item: A) => void,
    describe: () => string,
  ): Watcher<A> {
    const watcher = new Watcher<A>(`${this.options.label} ${label}`, onItem, describe);
    const abort = new AbortController();
    this.aborts.add(abort);
    const consume = Effect.flatMap(Effect.service(WsClientService), (client) =>
      Stream.runForEach(open(client), (item) => Effect.sync(() => watcher.push(item))),
    );
    void this.runtime.runPromiseExit(consume, { signal: abort.signal }).then((exit) => {
      this.aborts.delete(abort);
      watcher.finish(
        Exit.isSuccess(exit) ? { ok: true } : { ok: false, error: formatCause(exit.cause) },
      );
    });
    return watcher;
  }

  /** Interrupts every open stream (the client sends `Interrupt`) and closes the socket. */
  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    for (const abort of this.aborts) abort.abort();
    await this.runtime.dispose();
  }
}
