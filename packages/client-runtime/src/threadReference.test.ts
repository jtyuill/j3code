import { EnvironmentId, ThreadId } from "@t3tools/contracts";
import {
  decodeComposerContextFragment,
  encodeComposerContextFragment,
} from "@t3tools/shared/composerContextClipboard";
import { collectComposerContextReferences } from "@t3tools/shared/composerContextReferences";
import { describe, expect, it } from "vite-plus/test";

import { threadChipCopyTarget } from "./threadReference.ts";

describe("threadChipCopyTarget", () => {
  it("copies a thread chip that pastes back as the same thread on its machine", () => {
    const target = threadChipCopyTarget(
      { environmentId: EnvironmentId.make("env-1"), threadId: ThreadId.make("thread-1") },
      "Fix the login flow",
    );

    expect(target).toMatchObject({
      kind: "thread",
      description: "Fix the login flow",
      successTitle: "Thread copied",
    });
    const pasted = decodeComposerContextFragment(encodeComposerContextFragment(target.fragment!));
    expect(pasted).toEqual({
      version: 1,
      source: { environmentId: "env-1" },
      records: [
        expect.objectContaining({
          kind: "thread",
          environmentId: "env-1",
          threadId: "thread-1",
          title: "Fix the login flow",
        }),
      ],
    });
    // A paste keeps only the records its plain text references.
    expect(collectComposerContextReferences(target.value)).toEqual([
      expect.objectContaining({
        kind: "thread",
        contextId: pasted!.records[0]!.contextId,
        label: "Fix the login flow",
      }),
    ]);
  });
});
