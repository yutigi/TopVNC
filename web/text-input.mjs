// The on-screen keyboard field: text arrives as input events, which iOS
// reports reliably, unlike its key events. Between edits the field holds one
// local placeholder character with the caret after it, so Backspace has
// something to delete: iOS may emit no edit events for Backspace in an empty
// field.
//
// Each edit is sent exactly once. beforeinput sends an insertion only when
// cancelling it worked, so the field stays unchanged; otherwise the browser
// edits the field and the input event sends what the edit inserted.
// Composition (dictation, input methods) is left alone until it ends.

const BACKSPACE = 0xff08;
const DELETE = 0xffff;
// Edits sent as key taps rather than text. A word or line deletion is one
// key press: the field holds no text to measure it against.
const SPECIAL = {
  insertLineBreak: 0xff0d,
  insertParagraph: 0xff0d,
  deleteContentBackward: BACKSPACE,
  deleteWordBackward: BACKSPACE,
  deleteSoftLineBackward: BACKSPACE,
  deleteHardLineBackward: BACKSPACE,
  deleteContentForward: DELETE,
  deleteWordForward: DELETE,
  deleteSoftLineForward: DELETE,
  deleteHardLineForward: DELETE,
};
const DELETION = /^delete/;
const PLACEHOLDER = "​";

// `active()` says whether input is going to a host, `text(string)` types
// text, and `tap(keysym)` presses and releases one key.
export function attachTextInput(field, { active, text, tap }) {
  // The field and its selection before the current edit.
  let before;
  let composing = false;
  // A lifecycle release invalidates late edits from an abandoned IME session.
  // A fresh focus explicitly enables input again.
  let accepting = true;
  // An uncancelled deletion that beforeinput already sent, so its input
  // event must not send it again.
  let deletionSent = false;

  function snapshot() {
    before = { value: field.value, start: field.selectionStart, end: field.selectionEnd };
  }

  function resetField() {
    field.value = PLACEHOLDER;
    field.setSelectionRange(PLACEHOLDER.length, PLACEHOLDER.length);
    snapshot();
  }

  // Keep the caret after the placeholder: with the caret before it,
  // Backspace has nothing to delete again.
  function pinCaret() {
    if (composing || field.value !== PLACEHOLDER) return;
    if (field.selectionStart === PLACEHOLDER.length && field.selectionEnd === PLACEHOLDER.length) return;
    field.setSelectionRange(PLACEHOLDER.length, PLACEHOLDER.length);
    snapshot();
  }

  // Whether the field still holds everything that was outside the selection
  // before the edit, the placeholder included.
  function keptAround(value) {
    const head = before.value.slice(0, before.start);
    const tail = before.value.slice(before.end);
    return value.length >= head.length + tail.length && value.startsWith(head) && value.endsWith(tail);
  }

  // The text the edit inserted. It usually replaced the selection; otherwise
  // (a drop, say) take the span that changed.
  function insertedText() {
    const value = field.value;
    if (keptAround(value)) return value.slice(before.start, value.length - (before.value.length - before.end));
    let start = 0;
    while (start < before.value.length && start < value.length && before.value[start] === value[start]) start++;
    let end = before.value.length;
    let nextEnd = value.length;
    while (end > start && nextEnd > start && before.value[end - 1] === value[nextEnd - 1]) {
      end--;
      nextEnd--;
    }
    return value.slice(start, nextEnd);
  }

  // Whether a deletion will remove anything, and so be followed by input.
  function willDelete(inputType) {
    const { selectionStart: start, selectionEnd: end, value } = field;
    if (start !== end) return true;
    return inputType.endsWith("Backward") ? start > 0 : end < value.length;
  }

  function sendField() {
    const inserted = insertedText();
    if (inserted) text(inserted);
    resetField();
  }

  resetField();
  field.addEventListener("focus", () => { accepting = true; resetField(); });
  // The caret moves without edit events: tapping or dragging in the field,
  // the iOS space-bar trackpad, Home. Browsers differ on where they report
  // that, so listen to both, and to keyup as a fallback.
  field.addEventListener("selectionchange", pinCaret);
  field.ownerDocument?.addEventListener("selectionchange", () => {
    if (field.ownerDocument.activeElement === field) pinCaret();
  });
  field.addEventListener("keyup", pinCaret);

  field.addEventListener("beforeinput", (event) => {
    if (!accepting || !active()) return;
    deletionSent = false;
    if (event.isComposing) return;
    snapshot();
    const special = SPECIAL[event.inputType];
    const insertion = (event.inputType === "insertText" || event.inputType === "insertReplacementText") && event.data;
    if (!special && !insertion) return;
    event.preventDefault();
    const deletion = DELETION.test(event.inputType);
    if (!event.defaultPrevented && !deletion) {
      // Not cancelled: the input event sends what the browser inserts.
      return;
    }
    // Send deletions now even when they cannot be cancelled: one that
    // removes nothing is followed by no input event.
    if (special) tap(special);
    else text(event.data);
    if (event.defaultPrevented) pinCaret();
    else if (willDelete(event.inputType)) deletionSent = true;
    // Moving the caret before the browser applies the deletion could move
    // the deletion too: put it back once the edit is done.
    else setTimeout(pinCaret);
  });

  // Uncancelled insertions, and anything else that changed the field.
  field.addEventListener("input", (event) => {
    if (!accepting || !active()) return;
    const key = SPECIAL[event.inputType];
    const deletion = DELETION.test(event.inputType);
    if (event.isComposing) {
      // A deletion past the start of the composition removed the
      // placeholder: that press was meant for the host.
      if (deletion && !keptAround(field.value)) {
        if (key) tap(key);
        resetField();
      }
      return;
    }
    if (key && deletion) {
      if (!deletionSent) tap(key);
      deletionSent = false;
      resetField();
      return;
    }
    // Other edits, including deletions sent as no key, such as a cut:
    // send what they inserted, never the placeholder.
    sendField();
  });

  field.addEventListener("compositionstart", () => {
    if (!accepting || !active()) return;
    composing = true;
    snapshot();
  });

  // Browsers differ on whether an input event follows compositionend, and
  // whether it is still marked as composing; send the result here, once.
  field.addEventListener("compositionend", () => {
    composing = false;
    if (!accepting || !active()) return;
    sendField();
  });
  return {
    cancel() { accepting = false; composing = false; deletionSent = false; resetField(); },
  };
}
