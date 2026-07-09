export const arr = (v: unknown) => String(v ?? '').split(',').map((x) => x.trim()).filter(Boolean);
export const csv = (values: unknown) => Array.isArray(values) ? values.join(', ') : '';
export const shortTs = (t: unknown) => (typeof t === 'string' ? t.replace('T', ' ').slice(0, 19) : '');
export const timeOf = (env: any) => {
  const d = new Date(env?.ts ?? Date.now());
  return isNaN(d.getTime()) ? '--:--:--' : d.toTimeString().slice(0, 8);
};
export const relativeTime = (t: unknown) => {
  const d = new Date(String(t ?? ''));
  if (isNaN(d.getTime())) return '';
  const sec = Math.max(0, Math.floor((Date.now() - d.getTime()) / 1000));
  if (sec < 60) return 'now';
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hrs = Math.floor(min / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return days < 14 ? `${days}d ago` : shortTs(t).slice(0, 10);
};
export const summarize = (p: unknown, max = 110) => {
  if (p == null) return '';
  const s = typeof p === 'string' ? p : JSON.stringify(p);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
};
export const conversationLabel = (s: any) => s?.title || s?.preview || 'conversation';
export const uid = () => Math.random().toString(36).slice(2);
export function firstSentence(text: string) {
  const compact = String(text ?? '').replace(/\s+/g, ' ').trim();
  if (!compact) return '';
  const sentence = compact.match(/^(.{20,220}?[.!?])\s/)?.[1] ?? compact;
  return sentence.length > 180 ? `${sentence.slice(0, 177)}...` : sentence;
}
export function shortList(values: string[] = [], max = 2) {
  const list = values.filter(Boolean);
  if (!list.length) return '';
  const shown = list.slice(0, max).join(', ');
  return list.length > max ? `${shown}, +${list.length - max}` : shown;
}
