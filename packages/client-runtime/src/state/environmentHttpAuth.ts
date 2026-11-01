import * as Effect from "effect/Effect";
import {
  ORCHESTRATION_PROTOCOL_HEADER,
  ORCHESTRATION_PROTOCOL_VERSION_TEXT,
} from "@t3tools/contracts";

import type { PreparedConnection } from "../connection/model.ts";
import {
  executeEnvironmentHttpRequest,
  makeEnvironmentHttpApiGroupClient,
  makeEnvironmentHttpApiUrlBuilder,
  RemoteEnvironmentAuthTimeoutError,
  type RemoteEnvironmentRequestError,
} from "../rpc/http.ts";

/** The header the orchestration snapshot endpoints check before answering. */
export const orchestrationProtocolHeaders = {
  [ORCHESTRATION_PROTOCOL_HEADER]: ORCHESTRATION_PROTOCOL_VERSION_TEXT,
} as const;

/** j3 hosts take no credential, so requests go out without auth headers. */
export const executeEnvironmentHttpApiRequest = Effect.fn(
  "clientRuntime.state.executeEnvironmentHttpApiRequest",
)(function* <
  Group extends Parameters<typeof makeEnvironmentHttpApiGroupClient>[1],
  A,
  E,
  R,
>(input: {
  readonly prepared: PreparedConnection;
  /** The URL the request goes to, from the group's contract URL builder. */
  readonly url: (urls: ReturnType<typeof makeEnvironmentHttpApiUrlBuilder>[Group]) => string;
  readonly timeoutMs: number;
  readonly group: Group;
  readonly request: (input: {
    readonly client: Effect.Success<ReturnType<typeof makeEnvironmentHttpApiGroupClient<Group>>>;
  }) => Effect.Effect<A, E, R>;
}): Effect.fn.Return<
  A,
  RemoteEnvironmentRequestError,
  Effect.Services<ReturnType<typeof makeEnvironmentHttpApiGroupClient<Group>>> | R
> {
  const httpBaseUrl = input.prepared.httpBaseUrl;
  const requestUrl = input.url(makeEnvironmentHttpApiUrlBuilder(httpBaseUrl)[input.group]);
  const client = yield* makeEnvironmentHttpApiGroupClient(httpBaseUrl, input.group);
  return yield* executeEnvironmentHttpRequest(
    requestUrl,
    input.timeoutMs,
    input.request({ client }),
  ).pipe(
    Effect.timeoutOrElse({
      duration: input.timeoutMs,
      orElse: () => Effect.fail(new RemoteEnvironmentAuthTimeoutError(requestUrl, input.timeoutMs)),
    }),
  );
});
