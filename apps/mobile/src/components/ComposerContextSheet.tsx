import { Button } from "./ui/Button";
import type { ComposerContextRecord, EnvironmentId, ScopedThreadRef } from "@t3tools/contracts";
import { formatAttachmentSize } from "@t3tools/client-runtime/state/attachments";
import { videoMimeType } from "@t3tools/shared/video";
import { ScrollView, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import {
  composerAttachmentInlineUri,
  isFileBackedComposerAttachment,
  type DraftComposerAttachment,
} from "../lib/composerImages";
import { contextImageGallery } from "../lib/contextImageGallery";
import { FilePreviewModal } from "./FilePreviewModal";
import { VideoPreviewModal } from "./VideoPreviewModal";
import { ComposerContextAttachment } from "./ComposerContextAttachment";
import { AppText as Text } from "./ui/AppText";
import { Sheet } from "./ui/Sheet";

function ContextField(props: { label: string; value: string | null | undefined }) {
  if (!props.value) return null;
  return (
    <View className="gap-1">
      <Text className="text-xs text-foreground-muted">{props.label}</Text>
      <Text selectable className="text-base text-foreground">
        {props.value}
      </Text>
    </View>
  );
}

export function ComposerContextSheet(props: {
  readonly label: string;
  readonly record: ComposerContextRecord | undefined;
  readonly onClose: () => void;
  readonly onRemove?: () => void;
  readonly skillDescription?: string;
  readonly onOpenThread?: (thread: ScopedThreadRef) => void;
  readonly environmentId?: EnvironmentId;
  readonly records?: ReadonlyArray<ComposerContextRecord>;
  readonly attachments?: ReadonlyArray<DraftComposerAttachment>;
}) {
  const insets = useSafeAreaInsets();
  const record = props.record;
  const localAttachment =
    record && "attachmentId" in record
      ? props.attachments?.find((entry) => entry.id === record.attachmentId)
      : undefined;
  const localFile =
    localAttachment && isFileBackedComposerAttachment(localAttachment)
      ? localAttachment
      : undefined;
  if (record && !("payload" in record) && record.kind === "image") {
    const mimeType = videoMimeType(record) ?? record.mimeType;
    const resource = {
      _tag: "attachment" as const,
      attachmentId: record.attachmentId,
      fileName: record.name,
      mimeType,
    };
    const remoteSource = props.environmentId
      ? { environmentId: props.environmentId, resource }
      : null;
    if (videoMimeType(record)) {
      if (localFile?.type === "file") {
        return (
          <VideoPreviewModal
            source={{ type: "local", attachment: localFile }}
            onRequestClose={props.onClose}
          />
        );
      }
      if (remoteSource && !localFile) {
        return (
          <VideoPreviewModal
            source={{
              type: "media",
              name: record.name,
              mimeType,
              ...remoteSource,
              actionsSource: { name: record.name, mimeType, ...remoteSource },
            }}
            onRequestClose={props.onClose}
          />
        );
      }
    } else {
      const inlineUri = composerAttachmentInlineUri(localAttachment);
      const source = localFile
        ? { attachment: localFile }
        : inlineUri
          ? { uri: inlineUri }
          : remoteSource;
      if (source) {
        return (
          <FilePreviewModal
            source={{
              kind: "image",
              name: record.name,
              ...source,
              gallery: contextImageGallery(
                props.records ?? [],
                props.attachments ?? [],
                props.environmentId,
              ),
              actionsSource: { name: record.name, mimeType, ...source },
            }}
            onRequestClose={props.onClose}
          />
        );
      }
    }
  }
  const supported =
    record !== undefined &&
    !("payload" in record) &&
    (record.kind === "image" || record.kind === "thread" || record.kind === "skill");
  return (
    <Sheet title={props.label} onClose={props.onClose}>
      <ScrollView
        nestedScrollEnabled
        className="flex-1"
        contentInsetAdjustmentBehavior="automatic"
        contentContainerStyle={{
          paddingHorizontal: 20,
          paddingTop: 12,
          gap: 16,
          paddingBottom: Math.max(20, insets.bottom),
        }}
      >
        {!record ? (
          <Text className="text-foreground">
            Context unavailable. The reference was copied without its payload. Copy it again from
            the original message or remove it.
          </Text>
        ) : !supported ? (
          <Text className="text-foreground">
            This context type is not supported by this version of the app. Its payload will be
            preserved when sent.
          </Text>
        ) : (
          <>
            {record.kind === "image" ? (
              <ContextField
                label="File"
                value={`${record.mimeType} · ${formatAttachmentSize(record.sizeBytes)}`}
              />
            ) : null}
            {record.kind === "thread" ? (
              <View className="gap-3">
                <ContextField label="Thread" value={record.title} />
                {props.onOpenThread ? (
                  <Button
                    label="Open thread"
                    tone="secondary"
                    onPress={() =>
                      props.onOpenThread?.({
                        environmentId: record.environmentId,
                        threadId: record.threadId,
                      })
                    }
                    fullWidth
                  />
                ) : null}
              </View>
            ) : null}
            {record.kind === "skill" ? (
              <View className="gap-3">
                <ContextField label="Skill" value={record.name} />
                <ContextField
                  label="Description"
                  value={props.skillDescription ?? "No description is available for this skill."}
                />
              </View>
            ) : null}
            {record.kind === "image" ? (
              <ComposerContextAttachment
                key={JSON.stringify([
                  props.environmentId,
                  record.attachmentId,
                  props.attachments?.find((entry) => entry.id === record.attachmentId)?.fileUri,
                ])}
                record={record}
                environmentId={props.environmentId}
                attachment={props.attachments?.find((entry) => entry.id === record.attachmentId)}
              />
            ) : null}
          </>
        )}
        {props.onRemove ? (
          <Button label="Remove from draft" tone="secondary" onPress={props.onRemove} fullWidth />
        ) : null}
      </ScrollView>
    </Sheet>
  );
}
