import { EnvironmentHttpCommonError, OrchestrationV2ShellSnapshot } from "@t3tools/contracts";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { HttpClient, HttpClientError, type HttpClientResponse } from "effect/http";

import type { PreparedConnection } from "../connection/model.ts";
import {
  executeEnvironmentHttpApiRequest,
  orchestrationProtocolHeaders,
} from "./environmentHttpAuth.ts";

// On timeout the socket asks the same server for the same full snapshot, so a
// short deadline only throws the first build away. The socket fallback is for
// setups where /api fails but /ws works, such as a proxy that blocks /api.
const DEFAULT_SHELL_SNAPSHOT_TIMEOUT_MS = 20_000;

export const fetchEnvironmentShellSnapshot = Effect.fn(
  "clientRuntime.state.fetchEnvironmentShellSnapshot",
)(function* (input: {
  readonly prepared: PreparedConnection;
  readonly timeoutMs?: number;
  readonly omitIdleSubagents?: boolean;
  readonly omitOlderSettled?: boolean;
}) {
  const endpoint = {
    query: {
      ...(input.omitIdleSubagents === true ? { omitIdleSubagents: "1" } : {}),
      ...(input.omitOlderSettled === true ? { omitOlderSettled: "1" } : {}),
    },
  };
  return yield* executeEnvironmentHttpApiRequest({
    ...input,
    group: "orchestration",
    url: (urls) => urls.shellSnapshot(endpoint),
    timeoutMs: input.timeoutMs ?? DEFAULT_SHELL_SNAPSHOT_TIMEOUT_MS,
    request: ({ client }) =>
      client
        .shellSnapshot({
          headers: orchestrationProtocolHeaders,
          ...endpoint,
          responseMode: "response-only",
        })
        .pipe(Effect.flatMap(decodeShellSnapshotResponse)),
  });
});

const decodeShellSnapshotJson = Schema.decodeUnknownEffect(
  Schema.toCodecJson(OrchestrationV2ShellSnapshot),
);

const decodeEnvironmentHttpCommonError = Schema.decodeUnknownEffect(
  Schema.toCodecJson(EnvironmentHttpCommonError),
);

const decodeShellSnapshotResponse = (
  response: HttpClientResponse.HttpClientResponse,
): Effect.Effect<
  OrchestrationV2ShellSnapshot,
  HttpClientError.HttpClientError | Schema.SchemaError | EnvironmentHttpCommonError
> => {
  if (response.status === 200) {
    return response.json.pipe(Effect.flatMap(decodeShellSnapshotJson));
  }
  const statusError = new HttpClientError.HttpClientError({
    reason: new HttpClientError.StatusCodeError({ request: response.request, response }),
  });
  return response.json.pipe(
    Effect.flatMap(decodeEnvironmentHttpCommonError),
    Effect.matchEffect({
      onFailure: () => Effect.fail(statusError),
      onSuccess: Effect.fail,
    }),
  );
};

export class ShellSnapshotLoader extends Context.Service<
  ShellSnapshotLoader,
  {
    readonly load: (
      prepared: PreparedConnection,
      options?: { readonly omitIdleSubagents?: boolean; readonly omitOlderSettled?: boolean },
    ) => Effect.Effect<Option.Option<OrchestrationV2ShellSnapshot>>;
  }
>()("@t3tools/client-runtime/state/shellSnapshotHttp/ShellSnapshotLoader") {}

export const layer: Layer.Layer<ShellSnapshotLoader, never, HttpClient.HttpClient> = Layer.effect(
  ShellSnapshotLoader,
  Effect.gen(function* () {
    const httpClient = yield* HttpClient.HttpClient;
    return ShellSnapshotLoader.of({
      load: Effect.fn("ShellSnapshotLoader.load")(function (prepared, options) {
        return fetchEnvironmentShellSnapshot({
          prepared,
          ...(options?.omitIdleSubagents === true ? { omitIdleSubagents: true } : {}),
          ...(options?.omitOlderSettled === true ? { omitOlderSettled: true } : {}),
        }).pipe(
          Effect.map(Option.some<OrchestrationV2ShellSnapshot>),
          Effect.provideService(HttpClient.HttpClient, httpClient),
          Effect.catchCause((cause) =>
            Effect.logWarning(
              "Could not load the environment shell snapshot over HTTP; using the socket snapshot instead.",
            ).pipe(
              Effect.annotateLogs({ cause: Cause.pretty(cause) }),
              Effect.as(Option.none<OrchestrationV2ShellSnapshot>()),
            ),
          ),
        );
      }),
    });
  }),
);
