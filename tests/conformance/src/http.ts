import {
  type AuthSessionState,
  EnvironmentHttpApi,
  type ExecutionEnvironmentDescriptor,
} from "@t3tools/contracts";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import { FetchHttpClient, HttpClient } from "effect/http";
import * as HttpApiClient from "effect/http-api/HttpApiClient";

import { StepFailure, formatCause } from "./report.ts";

/** `CR/rpc/http.ts` remoteApiBaseUrl: the API lives at the origin root. */
export function remoteApiBaseUrl(httpBaseUrl: string): string {
  const url = new URL(httpBaseUrl);
  url.pathname = "/";
  url.search = "";
  url.hash = "";
  return url.toString();
}

/** Timeouts from `CR/environment/descriptor.ts` and `CR/state/session.ts`. */
const DESCRIPTOR_TIMEOUT_MS = 10_000;
const SESSION_STATE_TIMEOUT_MS = 6_000;

async function runHttp<A, E>(
  what: string,
  timeoutMs: number,
  request: Effect.Effect<A, E, HttpClient.HttpClient>,
): Promise<A> {
  const exit = await Effect.runPromiseExit(
    request.pipe(
      Effect.timeoutOrElse({
        duration: timeoutMs,
        orElse: () => Effect.die(new Error(`no response within ${timeoutMs / 1000}s`)),
      }),
      Effect.provide(FetchHttpClient.layer),
    ),
  );
  if (Exit.isSuccess(exit)) return exit.value;
  throw new StepFailure(`${what} failed:\n${formatCause(exit.cause)}`);
}

/**
 * The contracts' `EnvironmentHttpApi` client, as `CR/rpc/http.ts`
 * `makeEnvironmentHttpApiGroupClient` builds it. Responses decode with the contracts schemas.
 */
export const environmentHttp = (httpBaseUrl: string) => {
  const group = <G extends "metadata" | "auth">(name: G) =>
    Effect.flatMap(Effect.service(HttpClient.HttpClient), (httpClient) =>
      HttpApiClient.group(EnvironmentHttpApi, {
        httpClient,
        group: name,
        baseUrl: remoteApiBaseUrl(httpBaseUrl),
      }),
    );
  return {
    descriptor: (): Promise<ExecutionEnvironmentDescriptor> =>
      runHttp(
        "GET /.well-known/t3/environment",
        DESCRIPTOR_TIMEOUT_MS,
        Effect.flatMap(group("metadata"), (client) => client.descriptor()),
      ),
    // Credential-less connections send no auth headers (`CR/state/environmentHttpAuth.ts`).
    authSession: (): Promise<AuthSessionState> =>
      runHttp(
        "GET /api/auth/session",
        SESSION_STATE_TIMEOUT_MS,
        Effect.flatMap(group("auth"), (client) => client.session({ headers: {} })),
      ),
  };
};

/** Resolves a server-issued `relativeUrl` against the HTTP base (`CR/state/assets.ts`). */
export function resolveRelativeUrl(httpBaseUrl: string, relativeUrl: string): string {
  return new URL(relativeUrl, remoteApiBaseUrl(httpBaseUrl)).toString();
}

/**
 * The attachment upload has no HttpApi contract: the mobile app POSTs the raw bytes with the
 * image's content type and accepts any 2xx (`M/lib/attachmentUpload.ts`).
 */
export async function uploadBytes(
  url: string,
  bytes: Uint8Array<ArrayBuffer>,
  mimeType: string,
): Promise<number> {
  const response = await fetchOrFail(`POST ${url}`, url, {
    method: "POST",
    headers: { "content-type": mimeType },
    body: bytes,
  });
  if (response.status < 200 || response.status >= 300) {
    throw new StepFailure(
      `POST ${url} answered ${response.status}, expected 2xx; body: ${(await response.text()).slice(0, 500)}`,
    );
  }
  return response.status;
}

/** Images load by URL with no headers (`M/features/threads/ThreadMarkdownImage.tsx`). */
export async function downloadBytes(
  url: string,
): Promise<{ status: number; contentType: string | null; bytes: Uint8Array }> {
  const response = await fetchOrFail(`GET ${url}`, url, { method: "GET" });
  return {
    status: response.status,
    contentType: response.headers.get("content-type"),
    bytes: new Uint8Array(await response.arrayBuffer()),
  };
}

async function fetchOrFail(what: string, url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, { ...init, signal: AbortSignal.timeout(15_000) });
  } catch (error) {
    const cause = error instanceof Error && error.cause instanceof Error ? `: ${error.cause.message}` : "";
    throw new StepFailure(`${what} failed: ${String(error)}${cause}`);
  }
}
