// The on-screen keyboard field: text arrives as input events, which iOS
// reports reliably, unlike its key events. The field is kept empty between
// events.
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
// Deletions change nothing in the empty field, so no input event follows
// even when they cannot be cancelled: only beforeinput can send them.
const DELETION = /^delete/;

// `active()` says whether input is going to a host, `text(string)` types
// text, and `tap(keysym)` presses and releases one key.
export function attachTextInput(field, { active, text, tap }) {
  function sendField() {
    if (field.value) text(field.value);
    field.value = "";
  }

  field.addEventListener("beforeinput", (event) => {
    if (!active() || event.isComposing) return;
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
  });

  // Uncancelled insertions, and anything else that changed the field.
  field.addEventListener("input", (event) => {
    if (!active() || event.isComposing) return;
    sendField();
  });

  // Browsers differ on whether an input event follows compositionend, and
  // whether it is still marked as composing; send the result here, once.
  field.addEventListener("compositionend", () => {
    if (!active()) return;
    sendField();
  });
}
