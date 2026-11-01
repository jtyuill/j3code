import { describe, expect, it, vi } from "vite-plus/test";

import { createAudioPlaybackSession } from "./audioSession";

function player() {
  return { play: vi.fn(), pause: vi.fn() };
}

describe("shared playback session", () => {
  it("pauses the playing video when another one starts", async () => {
    const first = player();
    const second = player();
    const firstSession = createAudioPlaybackSession(first);
    const secondSession = createAudioPlaybackSession(second);

    await firstSession.play();
    expect(first.play).toHaveBeenCalledOnce();
    expect(firstSession.requested).toBe(true);

    await secondSession.play();
    expect(second.play).toHaveBeenCalledOnce();
    expect(first.pause).toHaveBeenCalled();
    expect(firstSession.requested).toBe(false);
    expect(secondSession.requested).toBe(true);
    secondSession.pause();
  });

  it("does not start a video paused while its source was still loading", async () => {
    const video = player();
    const session = createAudioPlaybackSession(video);
    const source = Promise.withResolvers<void>();

    const playing = session.play(() => source.promise);
    session.pause();
    source.resolve();
    await playing;

    expect(video.play).not.toHaveBeenCalled();
  });

  it("adopts native playback started from the system controls", () => {
    const first = player();
    const second = player();
    const firstSession = createAudioPlaybackSession(first);
    const secondSession = createAudioPlaybackSession(second);

    firstSession.playingChanged(true);
    secondSession.playingChanged(true);

    expect(first.pause).toHaveBeenCalled();
    expect(secondSession.requested).toBe(true);
    secondSession.pause();
  });
});
