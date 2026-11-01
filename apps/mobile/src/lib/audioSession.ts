type AudioSessionToken = {
  readonly onCancel?: () => void;
  readonly onRelease?: () => void;
  released: boolean;
};

// Only one video plays at a time; a new owner cancels the previous one.
let owner: AudioSessionToken | null = null;
let pending = Promise.resolve();

// Finish an old activation before releasing it or activating its successor,
// so a stale activation cannot report ownership of a released session.
function enqueue<T>(operation: () => Promise<T>): Promise<T> {
  const result = pending.then(operation);
  pending = result.then(
    () => {},
    () => {},
  );
  return result;
}

export function acquireAudioSession(onCancel?: () => void, onRelease?: () => void) {
  const token: AudioSessionToken = { onCancel, onRelease, released: false };
  const previous = owner;
  // Publish before cancelling the old player so its callbacks see the new owner.
  owner = token;
  if (previous && !previous.released) previous.onCancel?.();
  return token;
}

export function isAudioSessionOwner(token: AudioSessionToken): boolean {
  return owner === token && !token.released;
}

/** Resolves once queued releases settle; video configures its own native session. */
export function activateAudioSession(token: AudioSessionToken): Promise<boolean> {
  return enqueue(async () => isAudioSessionOwner(token));
}

export function releaseAudioSession(token: AudioSessionToken): Promise<void> {
  token.released = true;
  return enqueue(async () => {
    if (owner !== token) return;
    token.onRelease?.();
    if (owner === token) owner = null;
  });
}

export function createAudioPlaybackSession(
  player: { play(): void; pause(): void },
  onRequestedChange?: (requested: boolean) => void,
) {
  let token: AudioSessionToken | null = null;
  const stopPlayer = () => {
    // useVideoPlayer may have disposed the native ref during unmount.
    try {
      player.pause();
    } catch {}
  };
  const pause = () => {
    const previous = token;
    token = null;
    onRequestedChange?.(false);
    stopPlayer();
    if (previous) void releaseAudioSession(previous).catch(() => {});
  };
  const acquire = () => {
    const next = acquireAudioSession(pause, stopPlayer);
    token = next;
    onRequestedChange?.(true);
    return next;
  };
  return {
    pause,
    get requested() {
      return token !== null;
    },
    async play(prepare?: () => Promise<void>) {
      const current = token ?? acquire();
      try {
        await prepare?.();
        if (!(await activateAudioSession(current))) return;
        if (token !== current || !isAudioSessionOwner(current)) return;
        player.play();
      } catch (error) {
        if (token === current) pause();
        throw error;
      }
    },
    // A stopped clock is also reported during buffering and seeks. Native
    // pause intent must call pause() separately from playingChange.
    playingChanged(playing: boolean) {
      if (playing && token === null) acquire();
    },
  };
}
