import { describe, expect, it } from "@effect/vitest";

import { mapRemoteEnvironmentError } from "./errors.ts";
import { RemoteEnvironmentAuthFetchError, RemoteEnvironmentAuthTimeoutError } from "../rpc/http.ts";

describe("mapRemoteEnvironmentError", () => {
  it("retries a host that cannot be reached", () => {
    const error = new RemoteEnvironmentAuthFetchError({
      message: "Failed to fetch remote environment endpoint.",
      cause: new TypeError("Failed to fetch"),
    });
    expect(mapRemoteEnvironmentError(error)).toMatchObject({
      _tag: "ConnectionTransientError",
      reason: "network",
      detail: error.message,
      hop: "server",
    });
  });

  it("retries a host that timed out", () => {
    const error = new RemoteEnvironmentAuthTimeoutError("http://shieldy:3780", 10_000);
    expect(mapRemoteEnvironmentError(error)).toMatchObject({
      _tag: "ConnectionTransientError",
      reason: "timeout",
      hop: "server",
    });
  });
});
