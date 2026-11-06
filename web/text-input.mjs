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

  function resetField() {
    field.value = PLACEHOLDER;
    field.setSelectionRange(PLACEHOLDER.length, PLACEHOLDER.length);
  }

  function sendField() {
    const value = field.value.startsWith(PLACEHOLDER) ? field.value.slice(PLACEHOLDER.length) : field.value;
    if (value) text(value);
    resetField();
  }

  resetField();
  field.addEventListener("focus", resetField);

  field.addEventListener("beforeinput", (event) => {
    if (!active() || event.isComposing) return;
    deletionHandled = false;
    const special = SPECIAL[event.inputType];
    const insertion = (event.inputType === "insertText" || event.inputType === "insertReplacementText") && event.data;
    if (!special && !insertion) return;
    event.preventDefault();
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
    if (!active() || event.isComposing) return;
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
