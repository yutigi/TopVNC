// Local view transform: the remote framebuffer fitted to the viewer, then
// zoomed and panned on this device only. Nothing here changes what the host
// sends.

export const MAX_ZOOM = 5;

// The CSS size of the framebuffer fitted inside the viewer, keeping its
// aspect ratio.
export function fit(viewWidth, viewHeight, frameWidth, frameHeight) {
  if (!(viewWidth > 0 && viewHeight > 0 && frameWidth > 0 && frameHeight > 0)) {
    return { width: 0, height: 0, left: 0, top: 0 };
  }
  const scale = Math.min(viewWidth / frameWidth, viewHeight / frameHeight);
  const width = frameWidth * scale;
  const height = frameHeight * scale;
  return { width, height, left: (viewWidth - width) / 2, top: (viewHeight - height) / 2 };
}

export class View {
  constructor() {
    this.zoom = 1;
    this.x = 0;
    this.y = 0;
    this.base = fit(0, 0, 0, 0);
    this.viewWidth = 0;
    this.viewHeight = 0;
  }

  // The viewer or framebuffer size changed: refit and keep the view valid.
  layout(viewWidth, viewHeight, frameWidth, frameHeight) {
    this.viewWidth = viewWidth;
    this.viewHeight = viewHeight;
    this.base = fit(viewWidth, viewHeight, frameWidth, frameHeight);
    this.clamp();
  }

  reset() {
    this.zoom = 1;
    this.x = 0;
    this.y = 0;
  }

  // Zoom by `factor` keeping the viewer point (cx, cy) fixed.
  zoomAt(factor, cx, cy) {
    const zoom = Math.min(MAX_ZOOM, Math.max(1, this.zoom * factor));
    const applied = zoom / this.zoom;
    this.x = cx - (cx - this.x) * applied;
    this.y = cy - (cy - this.y) * applied;
    this.zoom = zoom;
    this.clamp();
  }

  pan(dx, dy) {
    this.x += dx;
    this.y += dy;
    this.clamp();
  }

  // Keep the zoomed framebuffer covering the viewer where it can.
  clamp() {
    if (this.zoom <= 1) {
      this.zoom = 1;
      this.x = 0;
      this.y = 0;
      return;
    }
    const { width, height, left, top } = this.base;
    const scaledWidth = width * this.zoom;
    const scaledHeight = height * this.zoom;
    // Translation applies after the fit offset; bound so no gap opens on a
    // side the zoomed image is larger than the viewer.
    const bound = (offset, size, view, base) => {
      if (size <= view) return (view - size) / 2 - base;
      return Math.min(0 - base, Math.max(view - size - base, offset));
    };
    this.x = bound(this.x, scaledWidth, this.viewWidth, left);
    this.y = bound(this.y, scaledHeight, this.viewHeight, top);
  }

  // CSS for the canvas: positioned at the fitted size, transformed from its
  // top-left corner.
  style() {
    const { width, height, left, top } = this.base;
    return {
      width: `${width}px`,
      height: `${height}px`,
      left: `${left}px`,
      top: `${top}px`,
      transform: `translate(${this.x}px, ${this.y}px) scale(${this.zoom})`,
    };
  }

  // Framebuffer coordinates of a viewer point, clamped to the framebuffer.
  toFrame(vx, vy, frameWidth, frameHeight) {
    const { width, height, left, top } = this.base;
    if (!(width > 0 && height > 0)) return { x: 0, y: 0 };
    const px = (vx - left - this.x) / this.zoom / width;
    const py = (vy - top - this.y) / this.zoom / height;
    const clampTo = (value, size) => Math.min(size - 1, Math.max(0, Math.floor(value * size)));
    return { x: clampTo(px, frameWidth), y: clampTo(py, frameHeight) };
  }

  // Viewer coordinates of a framebuffer point, for the local cursor.
  fromFrame(fx, fy, frameWidth, frameHeight) {
    const { width, height, left, top } = this.base;
    return {
      x: left + this.x + ((fx + 0.5) / frameWidth) * width * this.zoom,
      y: top + this.y + ((fy + 0.5) / frameHeight) * height * this.zoom,
    };
  }

  // Framebuffer pixels per viewer pixel, for relative trackpad motion.
  framePerViewPixel(frameWidth) {
    const { width } = this.base;
    return width > 0 ? frameWidth / (width * this.zoom) : 1;
  }
}
