// Input from the page, applied to one session's Connection. Kept apart from
// session.mjs, which loads the WebAssembly build, so it can be tested alone.
//
// Absolute pointer motion is coalesced under backpressure: only the latest
// position is kept, and it is retried on a timer until the socket drains.
// Buttons and keys are never dropped. Releasing everything discards held
// motion, so a stale button mask can never press a button again.

// Pointer motion is held back while this much is waiting to be sent.
export const MOTION_BACKLOG_BYTES = 64 * 1024;
// How often held motion is retried while the socket drains.
export const MOTION_RETRY_MS = 16;

// `connection` is the WebAssembly Connection, `socket` anything with a
// `bufferedAmount`, `send()` flushes the connection's output to the socket,
// and `fail(error)` closes the session after an error outside a page message.
export function createInput(connection, socket, { send, fail, timers = globalThis }) {
  let held;
  let retry;

  const congested = () => socket.bufferedAmount > MOTION_BACKLOG_BYTES;

  function discard() {
    held = undefined;
    if (retry !== undefined) timers.clearTimeout(retry);
    retry = undefined;
  }

  // Send held motion if the socket has drained, or try again shortly.
  function flush() {
    if (retry !== undefined) timers.clearTimeout(retry);
    retry = undefined;
    if (!held) return;
    if (congested()) {
      retry = timers.setTimeout(retried, MOTION_RETRY_MS);
      return;
    }
    const { buttons, x, y } = held;
    held = undefined;
    connection.pointer(buttons, x, y);
    send();
  }

  function retried() {
    retry = undefined;
    try {
      flush();
    } catch (error) {
      discard();
      fail(error);
    }
  }

  function handle(message) {
    switch (message.kind) {
      case "pointer":
        if (message.motion && congested()) {
          held = message;
          retry ??= timers.setTimeout(retried, MOTION_RETRY_MS);
          return;
        }
        // Newer than anything held, buttons included.
        discard();
        connection.pointer(message.buttons, message.x, message.y);
        break;
      case "motion":
        if (congested() && !message.buttonsChanged) return;
        // Relative motion carries its own button state; held absolute
        // motion is older.
        discard();
        connection.pointerMotion(message.buttons, message.dx, message.dy);
        break;
      case "code":
        connection.keyCode(message.code, message.down);
        break;
      case "keysym":
        connection.keySym(message.keysym, message.down);
        break;
      case "text":
        connection.typeText(message.text);
        break;
      case "release":
        discard();
        connection.releaseAll();
        break;
      case "quality":
        connection.setQuality(message.quality);
        break;
      case "serveScale":
        connection.requestServeScale(message.percent);
        break;
      case "serveDisplay":
        connection.requestServeDisplay(message.display);
        break;
      default:
        return;
    }
    send();
    // The socket may have drained since motion was held.
    if (held) flush();
  }

  return { handle, discard };
}
