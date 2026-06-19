export type LiveStatus = {
  kind: 'status';
  connected?: boolean;
  broker?: string;
  agent?: string;
};

export type LiveMessage = {
  kind: 'message';
  topic: string;
  env: any;
};

export type LiveEvent = LiveStatus | LiveMessage;

export function openLiveStream(onEvent: (event: LiveEvent) => void, onError: () => void): EventSource {
  const stream = new EventSource('/api/stream');
  stream.onmessage = (event) => {
    try {
      onEvent(JSON.parse(event.data));
    } catch {
      /* ignore malformed stream payloads */
    }
  };
  stream.onerror = onError;
  return stream;
}
