import test from "node:test";
import assert from "node:assert/strict";
import { attachTextInput } from "../text-input.mjs";

const RETURN = 0xff0d;
const BACKSPACE = 0xff08;

class Field extends EventTarget {
  value = "";
  selectionStart = 0;
  selectionEnd = 0;

  setSelectionRange(start, end) {
    this.selectionStart = start;
    this.selectionEnd = end;
  }
}

function fixture({ active = true } = {}) {
  const sent = [];
  const field = new Field();
  attachTextInput(field, {
    active: () => active,
    text: (text) => sent.push(["text", text]),
    tap: (keysym) => sent.push(["tap", keysym]),
  });
  return { field, sent, placeholder: field.value };
}

function fire(field, type, props = {}, cancelable = false) {
  const event = Object.assign(new Event(type, { cancelable }), props);
  field.dispatchEvent(event);
  return event;
}

// What a browser does for one edit: beforeinput, then, unless cancelled,
// the edit to the field and an input event if the field changed.
function edit(field, inputType, { data = null, cancelable = true, isComposing = false, ios = false } = {}) {
  // iOS may emit neither beforeinput nor input when there is nothing to delete.
  if (ios && inputType.startsWith("delete") && !field.value) return;
  const before = fire(field, "beforeinput", { inputType, data, isComposing }, cancelable);
  if (before.defaultPrevented) return;
  const previous = field.value;
  if (inputType.startsWith("delete")) field.value = field.value.slice(0, -1);
  else if (inputType === "insertLineBreak" || inputType === "insertParagraph") field.value += "\n";
  else if (inputType === "insertCompositionText") field.value = field.value.slice(0, field.selectionStart) + data;
  else field.value += data;
  if (field.value !== previous) fire(field, "input", { inputType, data, isComposing });
}

// Native insertions replace the current selection, rather than always append.
function insertAtSelection(field, inputType, data, { cancelable = true, isComposing = false, beforeinput = true, eventData = data } = {}) {
  if (beforeinput && fire(field, "beforeinput", { inputType, data: eventData, isComposing }, cancelable).defaultPrevented) return;
  const start = field.selectionStart;
  field.value = field.value.slice(0, start) + data + field.value.slice(field.selectionEnd);
  field.setSelectionRange(start + data.length, start + data.length);
  fire(field, "input", { inputType, data: eventData, isComposing });
}

test("inserted text is sent once whether or not the insertion can be cancelled", () => {
  for (const inputType of ["insertText", "insertReplacementText"]) {
    for (const cancelable of [true, false]) {
      const { field, sent, placeholder } = fixture();
      edit(field, inputType, { data: "hello", cancelable });
      assert.deepEqual(sent, [["text", "hello"]], `${inputType}, cancelable: ${cancelable}`);
      assert.equal(field.value, placeholder);
    }
  }
});

test("native insertions before the placeholder do not send it to the host", () => {
  for (const [inputType, data] of [["insertFromPaste", "hello"], ["insertText", "hello"], ["insertLineBreak", "\n"]]) {
    for (const beforeinput of [true, false]) {
      const { field, sent, placeholder } = fixture();
      field.setSelectionRange(0, 0);
      insertAtSelection(field, inputType, data, { cancelable: false, beforeinput, eventData: inputType === "insertLineBreak" ? null : data });
      assert.deepEqual(sent, [["text", data]]);
      assert.equal(field.value, placeholder);
      assert.equal(field.selectionStart, placeholder.length);
      assert.equal(field.selectionEnd, placeholder.length);
    }
  }
});

test("pasted zero-width characters are preserved on either side of the placeholder", () => {
  for (const position of [0, 1]) {
    for (const beforeinput of [true, false]) {
      for (const data of ["a\u200bb", "\u200bhello", "hello\u200b", "\u200b"]) {
        const { field, sent } = fixture();
        field.setSelectionRange(position, position);
        insertAtSelection(field, "insertFromPaste", data, { beforeinput });
        assert.deepEqual(sent, [["text", data]]);
      }
    }
  }
});

test("replacing the selected placeholder preserves zero-width characters in user text", () => {
  for (const data of ["a\u200bb", "\u200b"]) {
    for (const beforeinput of [true, false]) {
      const { field, sent } = fixture();
      field.setSelectionRange(0, field.value.length);
      insertAtSelection(field, "insertFromPaste", data, { beforeinput });
      assert.deepEqual(sent, [["text", data]]);
    }
  }
});

test("composition before the placeholder sends only the committed text", () => {
  const { field, sent, placeholder } = fixture();
  field.setSelectionRange(0, 0);
  fire(field, "compositionstart");
  insertAtSelection(field, "insertCompositionText", "に", { cancelable: false, isComposing: true });
  field.setSelectionRange(0, 1);
  insertAtSelection(field, "insertCompositionText", "日本", { cancelable: false, isComposing: true });
  assert.deepEqual(sent, []);
  fire(field, "compositionend", { data: "日本" });
  assert.deepEqual(sent, [["text", "日本"]]);
  assert.equal(field.value, placeholder);
});

test("a line break is one Return whether or not it can be cancelled", () => {
  for (const cancelable of [true, false]) {
    const { field, sent, placeholder } = fixture();
    edit(field, "insertLineBreak", { cancelable });
    // An uncancelled line break is typed as "\n", which the host receives
    // as Return.
    assert.deepEqual(sent, cancelable ? [["tap", RETURN]] : [["text", "\n"]]);
    assert.equal(field.value, placeholder);
  }
});

test("deletions reach the host even when they cannot be cancelled", () => {
  for (const [inputType, keysym] of [["deleteContentBackward", BACKSPACE], ["deleteWordBackward", BACKSPACE], ["deleteContentForward", 0xffff]]) {
    for (const cancelable of [true, false]) {
      const { field, sent, placeholder } = fixture();
      edit(field, inputType, { cancelable });
      assert.deepEqual(sent, [["tap", keysym]]);
      assert.equal(field.value, placeholder);
    }
  }
});

test("iOS Backspace works before typing and repeatedly after typing", () => {
  for (const cancelable of [true, false]) {
    const { field, sent, placeholder } = fixture();
    assert.ok(placeholder.length > 0);
    assert.equal(field.selectionStart, placeholder.length);
    edit(field, "deleteContentBackward", { ios: true, cancelable });
    edit(field, "insertText", { data: "hello", cancelable });
    for (let i = 0; i < 5; i++) {
      edit(field, "deleteContentBackward", { ios: true, cancelable });
      assert.equal(field.value, placeholder);
      assert.equal(field.selectionStart, placeholder.length);
      assert.equal(field.selectionEnd, placeholder.length);
    }
    assert.deepEqual(sent, [["tap", BACKSPACE], ["text", "hello"], ...Array.from({ length: 5 }, () => ["tap", BACKSPACE])]);
  }
});

test("input-only deletion is sent once and restores the placeholder", () => {
  const { field, sent, placeholder } = fixture();
  field.value = "";
  fire(field, "input", { inputType: "deleteContentBackward" });
  assert.deepEqual(sent, [["tap", BACKSPACE]]);
  assert.equal(field.value, placeholder);
});

test("focus restores the local placeholder without sending it", () => {
  const { field, sent, placeholder } = fixture();
  field.value = "stale local text";
  fire(field, "focus");
  fire(field, "input", { inputType: "insertText" });
  assert.equal(field.value, placeholder);
  assert.deepEqual(sent, []);
});

test("composed text is sent once when composition ends, input event or not", () => {
  // Chromium: the last input event is still composing, then compositionend.
  {
    const { field, sent, placeholder } = fixture();
    fire(field, "compositionstart");
    edit(field, "insertCompositionText", { data: "に", cancelable: false, isComposing: true });
    edit(field, "insertCompositionText", { data: "日本", cancelable: false, isComposing: true });
    assert.deepEqual(sent, []);
    fire(field, "compositionend", { data: "日本" });
    assert.deepEqual(sent, [["text", "日本"]]);
    assert.equal(field.value, placeholder);
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
