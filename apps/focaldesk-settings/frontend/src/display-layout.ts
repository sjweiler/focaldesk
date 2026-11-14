import type { OutputConfig } from './api';

export function logicalSize(output: OutputConfig): { width: number; height: number } {
  const rotated = output.transform === 'Rotate90' || output.transform === 'Rotate270';
  return { width: Math.round((rotated ? output.height : output.width) / output.scale),
    height: Math.round((rotated ? output.width : output.height) / output.scale) };
}
export function layoutBounds(outputs: OutputConfig[]) {
  if (!outputs.length) return { x: 0, y: 0, width: 1, height: 1 };
  const x = Math.min(...outputs.map(output => output.x));
  const y = Math.min(...outputs.map(output => output.y));
  return { x, y, width: Math.max(...outputs.map(output => output.x + logicalSize(output).width)) - x,
    height: Math.max(...outputs.map(output => output.y + logicalSize(output).height)) - y };
}
export function snapPosition(output: OutputConfig, others: OutputConfig[], x: number, y: number, threshold = 80) {
  const own = logicalSize(output);
  let sx = x, sy = y, dx = threshold, dy = threshold;
  for (const other of others) {
    const size = logicalSize(other);
    for (const candidate of [other.x, other.x + size.width, other.x - own.width, other.x + size.width - own.width]) {
      if (Math.abs(candidate - x) < dx) { dx = Math.abs(candidate - x); sx = candidate; }
    }
    for (const candidate of [other.y, other.y + size.height, other.y - own.height, other.y + size.height - own.height]) {
      if (Math.abs(candidate - y) < dy) { dy = Math.abs(candidate - y); sy = candidate; }
    }
  }
  return { x: Math.round(sx), y: Math.round(sy) };
}
