import test from "node:test";
import assert from "node:assert/strict";
import { attachTextInput } from "../text-input.mjs";

const RETURN = 0xff0d;
const BACKSPACE = 0xff08;

class Field extends EventTarget {
  value = "";
}

function fixture({ active = true } = {}) {
  const sent = [];
  const field = new Field();
  attachTextInput(field, {
    active: () => active,
    text: (text) => sent.push(["text", text]),
    tap: (keysym) => sent.push(["tap", keysym]),
  });
  return { field, sent };
}

function fire(field, type, props = {}, cancelable = false) {
  const event = Object.assign(new Event(type, { cancelable }), props);
  field.dispatchEvent(event);
  return event;
}

// What a browser does for one edit: beforeinput, then, unless cancelled,
// the edit to the field and an input event if the field changed.
function edit(field, inputType, { data = null, cancelable = true, isComposing = false } = {}) {
  const before = fire(field, "beforeinput", { inputType, data, isComposing }, cancelable);
  if (before.defaultPrevented) return;
  const previous = field.value;
  if (inputType.startsWith("delete")) field.value = field.value.slice(0, -1);
  else if (inputType === "insertLineBreak" || inputType === "insertParagraph") field.value += "\n";
  else if (inputType === "insertCompositionText") field.value = data;
  else field.value += data;
  if (field.value !== previous) fire(field, "input", { inputType, data, isComposing });
}

test("inserted text is sent once whether or not the insertion can be cancelled", () => {
  for (const inputType of ["insertText", "insertReplacementText"]) {
    for (const cancelable of [true, false]) {
      const { field, sent } = fixture();
      edit(field, inputType, { data: "hello", cancelable });
      assert.deepEqual(sent, [["text", "hello"]], `${inputType}, cancelable: ${cancelable}`);
      assert.equal(field.value, "");
    }
  }
});

test("a line break is one Return whether or not it can be cancelled", () => {
  for (const cancelable of [true, false]) {
    const { field, sent } = fixture();
    edit(field, "insertLineBreak", { cancelable });
    // An uncancelled line break is typed as "\n", which the host receives
    // as Return.
    assert.deepEqual(sent, cancelable ? [["tap", RETURN]] : [["text", "\n"]]);
    assert.equal(field.value, "");
  }
});

test("deletions reach the host even when they cannot be cancelled", () => {
  for (const cancelable of [true, false]) {
    const { field, sent } = fixture();
    edit(field, "deleteContentBackward", { cancelable });
    assert.deepEqual(sent, [["tap", BACKSPACE]]);
  }
});

test("composed text is sent once when composition ends, input event or not", () => {
  // Chromium: the last input event is still composing, then compositionend.
  {
    const { field, sent } = fixture();
    fire(field, "compositionstart");
    edit(field, "insertCompositionText", { data: "に", cancelable: false, isComposing: true });
    edit(field, "insertCompositionText", { data: "日本", cancelable: false, isComposing: true });
    assert.deepEqual(sent, []);
    fire(field, "compositionend", { data: "日本" });
    assert.deepEqual(sent, [["text", "日本"]]);
    assert.equal(field.value, "");
  }
  // WebKit: the committed text arrives in an input event that is no longer
  // composing, before compositionend.
  {
    const { field, sent } = fixture();
    fire(field, "compositionstart");
    edit(field, "insertCompositionText", { data: "日本", cancelable: false, isComposing: true });
    fire(field, "input", { inputType: "insertFromComposition", data: "日本", isComposing: false });
    fire(field, "compositionend", { data: "日本" });
    assert.deepEqual(sent, [["text", "日本"]]);
  }
  // Deleting within a composition edits the composition, not the host.
  {
    const { field, sent } = fixture();
    fire(field, "compositionstart");
    edit(field, "insertCompositionText", { data: "ab", cancelable: false, isComposing: true });
    edit(field, "deleteContentBackward", { cancelable: false, isComposing: true });
    fire(field, "compositionend", { data: "a" });
    assert.deepEqual(sent, [["text", "a"]]);
  }
});

test("nothing is sent without a connection", () => {
  const { field, sent } = fixture({ active: false });
  edit(field, "insertText", { data: "x" });
  edit(field, "deleteContentBackward");
  fire(field, "compositionend");
  assert.deepEqual(sent, []);
});
