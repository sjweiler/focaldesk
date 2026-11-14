// Convert amplitude to a useful visual range without presenting silence as activity.
export function meterPercent(amplitude: number): number {
  if (!Number.isFinite(amplitude) || amplitude <= 0) return 0;
  return Math.max(0, Math.min(100, (20 * Math.log10(Math.min(1, amplitude)) + 60) / 60 * 100));
}
export function volumeLabel(percent: number): string {
  return `${Math.round(percent)}%`;
}
