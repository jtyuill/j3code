import type { ComposerContextClipboardFragment, ScopedThreadRef } from "@t3tools/contracts";
import {
  formatComposerContextReference,
  threadContextRecord,
} from "@t3tools/shared/composerContextReferences";

export interface ThreadReferenceCopyTarget {
  readonly kind: "thread";
  /** The plain text: a thread chip's reference link. */
  readonly value: string;
  /** What a toast or palette row shows for the copy. */
  readonly description: string;
  /** T3's clipboard format, which pastes a thread back into a composer as a chip. */
  readonly fragment?: ComposerContextClipboardFragment;
  readonly clipboardTarget: string;
  readonly successTitle: string;
  readonly failureTitle: string;
}

/** A thread chip pastes into any composer whose agent reads that thread's machine. */
export function threadChipCopyTarget(
  threadRef: ScopedThreadRef,
  title: string,
): ThreadReferenceCopyTarget {
  const record = threadContextRecord(threadRef, title);
  return {
    kind: "thread",
    value: formatComposerContextReference(record),
    description: record.title,
    fragment: { version: 1, source: { environmentId: threadRef.environmentId }, records: [record] },
    clipboardTarget: "thread",
    successTitle: "Thread copied",
    failureTitle: "Failed to copy thread",
  };
}
