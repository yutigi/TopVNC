// The on-screen keyboard field: text arrives as input events, which iOS
// reports reliably, unlike its key events. Keep a local placeholder so iOS
// has something to delete: Backspace on an empty field may emit no edit events.
//
// Each edit is sent exactly once. beforeinput sends an insertion only when
// cancelling it worked, so the field stays unchanged; otherwise the browser
// edits the field and the input event sends its contents. Composition
// (dictation, input methods) is left alone until it ends.

// Edits sent as key taps rather than text.
const SPECIAL = {
  insertLineBreak: 0xff0d,
  insertParagraph: 0xff0d,
  deleteContentBackward: 0xff08,
  deleteWordBackward: 0xff08,
  deleteContentForward: 0xffff,
};
const DELETION = /^delete/;
const PLACEHOLDER = "\u200b";

// `active()` says whether input is going to a host, `text(string)` types
// text, and `tap(keysym)` presses and releases one key.
export function attachTextInput(field, { active, text, tap }) {
  let deletionHandled = false;
  let placeholderIndex = 0;
  let previousValue = PLACEHOLDER;
  let editSelection = null;

  function resetField() {
    field.value = PLACEHOLDER;
    field.setSelectionRange(PLACEHOLDER.length, PLACEHOLDER.length);
    placeholderIndex = 0;
    previousValue = PLACEHOLDER;
    editSelection = null;
  }

  function trackPlaceholder(event) {
    // Track the injected character, preserving U+200B in the user's own text.
    const value = field.value;
    let selection = editSelection?.inputType === event?.inputType ? editSelection : null;
    editSelection = null;
    if (placeholderIndex !== null) {
      // Input-only insertions still report the inserted text and final caret.
      if (!selection && typeof event?.data === "string") {
        const start = field.selectionStart - event.data.length;
        const end = start + previousValue.length + event.data.length - value.length;
        if (start >= 0 && end >= start && end <= previousValue.length) selection = { start, end };
      }
      if (selection) {
        if (selection.start <= placeholderIndex && selection.end > placeholderIndex) placeholderIndex = null;
        else if (selection.end <= placeholderIndex) placeholderIndex += value.length - previousValue.length;
      } else {
        // Without edit metadata, map the placeholder through the changed span.
        let start = 0;
        while (start < previousValue.length && start < value.length && previousValue[start] === value[start]) start++;
        let end = previousValue.length;
        let nextEnd = value.length;
        while (end > start && nextEnd > start && previousValue[end - 1] === value[nextEnd - 1]) {
          end--;
          nextEnd--;
        }
        if (placeholderIndex >= end) placeholderIndex += value.length - previousValue.length;
        else if (placeholderIndex >= start) placeholderIndex = null;
      }
      if (placeholderIndex !== null && value[placeholderIndex] !== PLACEHOLDER) placeholderIndex = null;
    }
    previousValue = value;
  }

  function sendField() {
    trackPlaceholder();
    const value = placeholderIndex === null
      ? field.value
      : field.value.slice(0, placeholderIndex) + field.value.slice(placeholderIndex + PLACEHOLDER.length);
    if (value) text(value);
    resetField();
  }

  resetField();
  field.addEventListener("focus", resetField);

  field.addEventListener("beforeinput", (event) => {
    if (!active()) return;
    editSelection = { start: field.selectionStart, end: field.selectionEnd, inputType: event.inputType };
    if (event.isComposing) return;
    deletionHandled = false;
    const special = SPECIAL[event.inputType];
    const insertion = (event.inputType === "insertText" || event.inputType === "insertReplacementText") && event.data;
    if (!special && !insertion) return;
    event.preventDefault();
    if (event.defaultPrevented) editSelection = null;
    if (!event.defaultPrevented && !(special && DELETION.test(event.inputType))) {
      // Not cancelled: the input event sends what the browser inserts.
      return;
    }
    if (special) tap(special);
    else text(event.data);
    deletionHandled = Boolean(special && DELETION.test(event.inputType) && !event.defaultPrevented);
  });

  // Uncancelled insertions, and anything else that changed the field.
  field.addEventListener("input", (event) => {
    if (!active()) return;
    trackPlaceholder(event);
    if (event.isComposing) return;
    // An uncancelled deletion produces input too. Send it only if beforeinput
    // did not already do so, then restore the placeholder for the next press.
    if (SPECIAL[event.inputType] && DELETION.test(event.inputType)) {
      if (!deletionHandled) tap(SPECIAL[event.inputType]);
      deletionHandled = false;
      resetField();
      return;
    }
    sendField();
  });

  // Browsers differ on whether an input event follows compositionend, and
  // whether it is still marked as composing; send the result here, once.
  field.addEventListener("compositionend", () => {
    if (!active()) return;
    sendField();
  });
}
