import type { ThreadId } from "@t3tools/contracts";
import * as Effect from "effect/Effect";

import type { PreparedConnection } from "../connection/model.ts";
import {
  executeEnvironmentHttpApiRequest,
  orchestrationProtocolHeaders,
} from "./environmentHttpAuth.ts";

export const fetchEnvironmentThreadTranscript = Effect.fn(
  "clientRuntime.state.fetchEnvironmentThreadTranscript",
)(function* (input: {
  readonly prepared: PreparedConnection;
  readonly threadId: ThreadId;
}) {
  return yield* executeEnvironmentHttpApiRequest({
    ...input,
    group: "orchestration",
    url: (urls) => urls.threadTranscript({ params: { threadId: input.threadId } }),
    timeoutMs: 60_000,
    request: ({ client }) =>
      client.threadTranscript({
        params: { threadId: input.threadId },
        headers: orchestrationProtocolHeaders,
      }),
  });
});
