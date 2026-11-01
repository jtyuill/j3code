import { EnvironmentId, ThreadId } from "@t3tools/contracts";
import { describe, expect, it } from "vite-plus/test";

import { assetImageCacheKey, assetImageSource } from "./assetImageSource";

const environmentId = EnvironmentId.make("environment-1");

describe("assetImageSource", () => {
  it("caches an attachment under one key across re-signed URLs", () => {
    const resource = { _tag: "attachment", attachmentId: "thread-1-photo.jpg" } as const;
    const first = assetImageSource(
      environmentId,
      resource,
      "https://host/api/assets/token-a/p.jpg",
    );
    const second = assetImageSource(
      environmentId,
      resource,
      "https://host/api/assets/token-b/p.jpg",
    );

    expect(first.cacheKey).toBeDefined();
    expect(second).toStrictEqual({
      uri: "https://host/api/assets/token-b/p.jpg",
      cacheKey: first.cacheKey,
    });
    expect(
      assetImageSource(EnvironmentId.make("environment-2"), resource, "https://other/p.jpg")
        .cacheKey,
    ).not.toBe(first.cacheKey);
  });

  it("keeps ids that contain colons from sharing a key", () => {
    const uri = "https://host/api/assets/token/p.jpg";
    const left = assetImageSource(
      EnvironmentId.make("a"),
      { _tag: "attachment", attachmentId: "b:c" },
      uri,
    );
    const right = assetImageSource(
      EnvironmentId.make("a:b"),
      { _tag: "attachment", attachmentId: "c" },
      uri,
    );

    expect(left.cacheKey).not.toBe(right.cacheKey);
  });

  it("keys host files by the revision the server read, and by URL without one", () => {
    const resource = {
      _tag: "media-file",
      threadId: ThreadId.make("thread-1"),
      path: "docs/shot.png",
    } as const;
    const first = assetImageSource(
      environmentId,
      resource,
      "https://host/api/assets/token-a/shot.png",
      "1:2:5:1000:1000",
    );

    expect(first.cacheKey).toBeDefined();
    expect(
      assetImageSource(
        environmentId,
        resource,
        "https://host/api/assets/token-b/shot.png",
        "1:2:5:1000:1000",
      ),
    ).toStrictEqual({ uri: "https://host/api/assets/token-b/shot.png", cacheKey: first.cacheKey });
    expect(assetImageCacheKey(environmentId, resource, "1:2:5:2000:2000")).not.toBe(first.cacheKey);
    expect(
      assetImageCacheKey(environmentId, { ...resource, path: "docs/other.png" }, "1:2:5:1000:1000"),
    ).not.toBe(first.cacheKey);
    expect(
      assetImageCacheKey(EnvironmentId.make("environment-2"), resource, "1:2:5:1000:1000"),
    ).not.toBe(first.cacheKey);
    expect(
      assetImageSource(
        environmentId,
        { _tag: "draft-workspace-file", cwd: "/repo", path: "logo.png" },
        "https://host/api/assets/token-a/logo.png",
      ),
    ).toStrictEqual({ uri: "https://host/api/assets/token-a/logo.png" });
  });
});
