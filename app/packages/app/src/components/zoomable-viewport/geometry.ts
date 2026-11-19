export interface ViewportSize {
  width: number;
  height: number;
}

export interface ViewportPoint {
  x: number;
  y: number;
}

export interface ViewportTransform {
  scale: number;
  x: number;
  y: number;
}

export interface ViewportScaleLimits {
  minScale: number;
  maxScale: number;
}

export interface ViewportFitOptions {
  padding?: number;
  maxWidth?: number;
  maxHeight?: number;
}

export const FIT_TRANSFORM: ViewportTransform = { scale: 1, x: 0, y: 0 };

/** The long side a picture is laid out at while only its small rendition is known. */
const PROVISIONAL_LONG_SIDE = 2048;

/**
 * The size to lay a picture out at from its small rendition's size, before the full one has come: the same shape,
 * its long side at `PROVISIONAL_LONG_SIDE`. The fit on screen depends on the shape alone, so nothing moves when the
 * real size arrives. Null for a size that is no size.
 */
export function provisionalContentSize(small: ViewportSize): ViewportSize | null {
  if (!(small.width > 0) || !(small.height > 0)) return null;
  const scale = PROVISIONAL_LONG_SIDE / Math.max(small.width, small.height);
  return { width: Math.round(small.width * scale), height: Math.round(small.height * scale) };
}

export function isActivePinchUpdate(activeTouches: number, reportedPointers: number): boolean {
  "worklet";
  return activeTouches >= 2 && reportedPointers >= 2;
}

export function isPointInsideTransformedContent(input: {
  point: ViewportPoint;
  transform: ViewportTransform;
  fittedContent: ViewportSize;
  viewport: ViewportSize;
}): boolean {
  "worklet";
  const centerX = input.viewport.width / 2 + input.transform.x;
  const centerY = input.viewport.height / 2 + input.transform.y;
  const halfWidth = (input.fittedContent.width * input.transform.scale) / 2;
  const halfHeight = (input.fittedContent.height * input.transform.scale) / 2;
  return (
    input.point.x >= centerX - halfWidth &&
    input.point.x <= centerX + halfWidth &&
    input.point.y >= centerY - halfHeight &&
    input.point.y <= centerY + halfHeight
  );
}

export function fitContentSize(
  content: ViewportSize,
  viewport: ViewportSize,
  options: ViewportFitOptions = {},
): ViewportSize {
  const padding = options.padding ?? 0;
  const availableWidth = Math.max(
    0,
    Math.min(viewport.width - padding * 2, options.maxWidth ?? Number.POSITIVE_INFINITY),
  );
  const availableHeight = Math.max(
    0,
    Math.min(viewport.height - padding * 2, options.maxHeight ?? Number.POSITIVE_INFINITY),
  );
  const fitScale = Math.min(availableWidth / content.width, availableHeight / content.height);
  return {
    width: content.width * fitScale,
    height: content.height * fitScale,
  };
}

function clampScale(scale: number, limits: ViewportScaleLimits): number {
  return Math.min(limits.maxScale, Math.max(limits.minScale, scale));
}

function clampOffset(offset: number, maximum: number): number {
  if (maximum === 0) {
    return 0;
  }
  return Math.min(maximum, Math.max(-maximum, offset));
}

export function clampTransform(
  transform: ViewportTransform,
  fittedContent: ViewportSize,
  viewport: ViewportSize,
  limits: ViewportScaleLimits,
): ViewportTransform {
  const scale = clampScale(transform.scale, limits);
  const maxX = Math.max(0, (fittedContent.width * scale - viewport.width) / 2);
  const maxY = Math.max(0, (fittedContent.height * scale - viewport.height) / 2);
  return {
    scale,
    x: clampOffset(transform.x, maxX),
    y: clampOffset(transform.y, maxY),
  };
}

export function zoomContentAtPoint(input: {
  transform: ViewportTransform;
  scale: number;
  focalPoint: ViewportPoint;
  fittedContent: ViewportSize;
  viewport: ViewportSize;
  limits: ViewportScaleLimits;
}): ViewportTransform {
  const scale = clampScale(input.scale, input.limits);
  const viewportCenter = {
    x: input.viewport.width / 2,
    y: input.viewport.height / 2,
  };
  const contentPoint = {
    x: (input.focalPoint.x - viewportCenter.x - input.transform.x) / input.transform.scale,
    y: (input.focalPoint.y - viewportCenter.y - input.transform.y) / input.transform.scale,
  };
  return clampTransform(
    {
      scale,
      x: input.focalPoint.x - viewportCenter.x - contentPoint.x * scale,
      y: input.focalPoint.y - viewportCenter.y - contentPoint.y * scale,
    },
    input.fittedContent,
    input.viewport,
    input.limits,
  );
}

export function panContent(input: {
  transform: ViewportTransform;
  delta: ViewportPoint;
  fittedContent: ViewportSize;
  viewport: ViewportSize;
  limits: ViewportScaleLimits;
}): ViewportTransform {
  return clampTransform(
    {
      ...input.transform,
      x: input.transform.x + input.delta.x,
      y: input.transform.y + input.delta.y,
    },
    input.fittedContent,
    input.viewport,
    input.limits,
  );
}
