// Touch gestures, interpreted from pointer events into viewer actions.
//
// Trackpad mode (default): one finger moves the remote pointer relatively,
// a tap clicks, tap-then-drag holds the left button; two fingers tap for a
// right click and drag to scroll. Direct mode: the pointer goes where the
// finger touches; a tap clicks there, dragging holds the left button, and a
// long press right-clicks. In both modes, a pinch zooms and pans the local
// view instead of scrolling.

export const TAP_MS = 250;
export const DOUBLE_TAP_MS = 300;
export const LONG_PRESS_MS = 550;
export const SLOP_PX = 8;
// Viewer pixels per wheel notch for two-finger scrolling.
export const SCROLL_STEP_PX = 28;
// Relative distance change that turns a two-finger gesture into a pinch.
export const PINCH_RATIO = 0.08;

const distance = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);

export class TouchGestures {
  // `actions` receives: move({x, y} | {dx, dy}), button(name, down),
  // scroll(dx, dy) in notches, zoom(factor, cx, cy), pan(dx, dy), and
  // released() when a gesture ends with nothing held.
  constructor(actions, mode = "trackpad") {
    this.actions = actions;
    this.mode = mode;
    this.touches = new Map();
    this.reset();
    this.lastTapEnd = -Infinity;
  }

  reset() {
    this.touches?.clear();
    this.gesture = undefined;
    this.held = undefined;
    this.longPress = undefined;
  }

  setMode(mode) {
    this.cancel();
    this.mode = mode;
  }

  // Abandon the gesture and release anything it holds, as on pointercancel,
  // page hiding, or a mode change.
  cancel() {
    clearTimeout(this.longPress);
    if (this.held) this.actions.button(this.held, false);
    this.reset();
    this.actions.released?.();
  }

  down(id, x, y, time) {
    if (this.touches.size >= 2) return;
    this.touches.set(id, { x, y, startX: x, startY: y, startTime: time });
    if (this.touches.size === 1) {
      const continuing = this.mode === "trackpad" && time - this.lastTapEnd < DOUBLE_TAP_MS;
      this.gesture = { kind: "one", moved: false, drag: continuing, start: time };
      if (this.mode === "direct") {
        this.actions.move({ x, y });
        clearTimeout(this.longPress);
        this.longPress = setTimeout(() => this.#longPress(), LONG_PRESS_MS);
      }
    } else {
      clearTimeout(this.longPress);
      // A second finger ends any one-finger action without clicking.
      if (this.held) {
        this.actions.button(this.held, false);
        this.held = undefined;
      }
      const [a, b] = [...this.touches.values()];
      this.gesture = {
        kind: "two",
        start: time,
        moved: false,
        pinch: false,
        distance: Math.max(1, distance(a, b)),
        startDistance: Math.max(1, distance(a, b)),
        centerX: (a.x + b.x) / 2,
        centerY: (a.y + b.y) / 2,
        startX: (a.x + b.x) / 2,
        startY: (a.y + b.y) / 2,
        // Notches already sent.
        scrolledX: 0,
        scrolledY: 0,
      };
    }
  }

  #longPress() {
    const gesture = this.gesture;
    if (gesture?.kind !== "one" || gesture.moved || this.mode !== "direct") return;
    gesture.longPressed = true;
    this.actions.button("right", true);
    this.actions.button("right", false);
  }

  move(id, x, y) {
    const touch = this.touches.get(id);
    if (!touch || !this.gesture) return;
    const previous = { x: touch.x, y: touch.y };
    touch.x = x;
    touch.y = y;
    const gesture = this.gesture;
    if (gesture.kind === "one") {
      if (!gesture.moved && Math.hypot(x - touch.startX, y - touch.startY) < SLOP_PX) return;
      if (!gesture.moved) {
        gesture.moved = true;
        clearTimeout(this.longPress);
        if ((gesture.drag || this.mode === "direct") && !gesture.longPressed) {
          this.held = "left";
          this.actions.button("left", true);
        }
      }
      if (this.mode === "direct") this.actions.move({ x, y });
      else this.actions.move({ dx: x - previous.x, dy: y - previous.y });
      return;
    }
    // One finger of a two-finger gesture lifted; the other does nothing.
    if (this.touches.size < 2) return;
    const [a, b] = [...this.touches.values()];
    const centerX = (a.x + b.x) / 2;
    const centerY = (a.y + b.y) / 2;
    const span = Math.max(1, distance(a, b));
    // Fingers report one at a time, so compare with the span at the start,
    // not the previous event.
    if (!gesture.pinch && Math.abs(span / gesture.startDistance - 1) > PINCH_RATIO) gesture.pinch = true;
    if (!gesture.moved && Math.hypot(centerX - gesture.startX, centerY - gesture.startY) >= SLOP_PX) {
      gesture.moved = true;
    }
    if (gesture.pinch) {
      this.actions.zoom(span / gesture.distance, centerX, centerY);
      this.actions.pan(centerX - gesture.centerX, centerY - gesture.centerY);
      gesture.moved = true;
    } else if (gesture.moved) {
      // Content follows the fingers: moving them up scrolls down. Count from
      // where the gesture started, so the slop distance is not lost.
      const stepsX = Math.trunc((gesture.startX - centerX) / SCROLL_STEP_PX) - gesture.scrolledX;
      const stepsY = Math.trunc((gesture.startY - centerY) / SCROLL_STEP_PX) - gesture.scrolledY;
      if (stepsX || stepsY) {
        gesture.scrolledX += stepsX;
        gesture.scrolledY += stepsY;
        this.actions.scroll(stepsX, stepsY);
      }
    }
    gesture.distance = span;
    gesture.centerX = centerX;
    gesture.centerY = centerY;
  }

  up(id, x, y, time) {
    const touch = this.touches.get(id);
    if (!touch) return;
    touch.x = x;
    touch.y = y;
    const gesture = this.gesture;
    this.touches.delete(id);
    if (!gesture) return;
    if (gesture.kind === "one") {
      clearTimeout(this.longPress);
      if (this.held) {
        this.actions.button(this.held, false);
        this.held = undefined;
      } else if (!gesture.moved && !gesture.longPressed && time - gesture.start <= TAP_MS) {
        if (this.mode === "direct") this.actions.move({ x, y });
        this.actions.button("left", true);
        this.actions.button("left", false);
        this.lastTapEnd = time;
        this.gesture = undefined;
        this.actions.released?.();
        return;
      }
      this.lastTapEnd = -Infinity;
      this.gesture = undefined;
      this.actions.released?.();
      return;
    }
    // The first finger of a two-finger gesture lifted.
    if (this.touches.size === 1) {
      gesture.lifted = gesture.lifted ?? time;
      return;
    }
    if (!gesture.moved && !gesture.pinch && time - gesture.start <= TAP_MS * 1.6) {
      this.actions.button("right", true);
      this.actions.button("right", false);
    }
    this.gesture = undefined;
    this.lastTapEnd = -Infinity;
    this.actions.released?.();
  }
}
