import test from "node:test";
import assert from "node:assert/strict";
import { attachTextInput } from "../text-input.mjs";

const RETURN = 0xff0d;
const BACKSPACE = 0xff08;
const DELETE = 0xffff;

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
// the edit at the caret and an input event if the field changed.
function edit(field, inputType, { data = null, cancelable = true, isComposing = false, ios = false } = {}) {
  // iOS may emit neither beforeinput nor input when there is nothing to delete.
  if (ios && inputType.startsWith("delete") && !field.value) return;
  const before = fire(field, "beforeinput", { inputType, data, isComposing }, cancelable);
  if (before.defaultPrevented) return;
  const previous = field.value;
  let start = field.selectionStart;
  let end = field.selectionEnd;
  let inserted = data ?? "";
  if (inputType.startsWith("delete")) {
    inserted = "";
    if (start === end && inputType.endsWith("Backward")) start = Math.max(0, start - 1);
    else if (start === end && inputType.endsWith("Forward")) end = Math.min(previous.length, end + 1);
  } else if (inputType === "insertLineBreak" || inputType === "insertParagraph") {
    inserted = "\n";
  } else if (inputType === "insertCompositionText") {
    // The composition replaces itself, from where it started.
    field.compositionFrom ??= start;
    start = field.compositionFrom;
  }
  field.value = previous.slice(0, start) + inserted + previous.slice(end);
  field.setSelectionRange(start + inserted.length, start + inserted.length);
  if (field.value !== previous) fire(field, "input", { inputType, data, isComposing });
}

// The caret moved without an edit, which browsers report as selectionchange.
function moveCaret(field, position, report = "selectionchange") {
  field.setSelectionRange(position, position);
  fire(field, report);
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
      // Without beforeinput nothing records where the edit started, and the
      // caret does not stay before the placeholder (see below).
      if (!beforeinput && position === 0) continue;
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
    // Without beforeinput nothing records the selection, and the caret does
    // not stay on the placeholder (see below).
    for (const beforeinput of data === "\u200b" ? [true] : [true, false]) {
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
  for (const [inputType, keysym] of [
    ["deleteContentBackward", BACKSPACE],
    ["deleteWordBackward", BACKSPACE],
    ["deleteSoftLineBackward", BACKSPACE],
    ["deleteHardLineBackward", BACKSPACE],
    ["deleteContentForward", DELETE],
    ["deleteWordForward", DELETE],
    ["deleteSoftLineForward", DELETE],
    ["deleteHardLineForward", DELETE],
  ]) {
    for (const cancelable of [true, false]) {
      const { field, sent, placeholder } = fixture();
      edit(field, inputType, { cancelable });
      assert.deepEqual(sent, [["tap", keysym]], `${inputType}, cancelable: ${cancelable}`);
      assert.equal(field.value, placeholder);
      assert.equal(field.selectionStart, placeholder.length);
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

test("the caret is kept after the placeholder so iOS Backspace still reaches the host", () => {
  for (const report of ["selectionchange", "keyup"]) {
    for (const cancelable of [true, false]) {
      const { field, sent, placeholder } = fixture();
      moveCaret(field, 0, report);
      assert.equal(field.selectionStart, placeholder.length);
      edit(field, "deleteContentBackward", { ios: true, cancelable });
      assert.deepEqual(sent, [["tap", BACKSPACE]]);
      assert.equal(field.value, placeholder);
    }
  }
});

test("a deletion with the caret before the placeholder puts the caret back", async () => {
  // As when the caret moved and the browser reported nothing.
  for (const cancelable of [true, false]) {
    const { field, sent, placeholder } = fixture();
    field.setSelectionRange(0, 0);
    edit(field, "deleteContentBackward", { ios: true, cancelable });
    // An uncancelled deletion still applies where the caret was.
    assert.equal(field.value, placeholder);
    await new Promise((resolve) => setTimeout(resolve));
    assert.equal(field.selectionStart, placeholder.length);
    edit(field, "deleteContentBackward", { ios: true, cancelable });
    assert.deepEqual(sent, [["tap", BACKSPACE], ["tap", BACKSPACE]]);
    assert.equal(field.value, placeholder);
  }
});

test("a deletion that removes nothing does not swallow the next input-only deletion", () => {
  // Forward delete with the caret at the end: no input event follows.
  {
    const { field, sent } = fixture();
    edit(field, "deleteContentForward", { cancelable: false });
    field.value = "";
    fire(field, "input", { inputType: "deleteContentBackward" });
    assert.deepEqual(sent, [["tap", DELETE], ["tap", BACKSPACE]]);
  }
  // A deletion whose input event never came, then a composing beforeinput.
  {
    const { field, sent } = fixture();
    fire(field, "beforeinput", { inputType: "deleteContentBackward" });
    fire(field, "beforeinput", { inputType: "insertCompositionText", data: "a", isComposing: true });
    field.value = "";
    fire(field, "input", { inputType: "deleteContentBackward" });
    assert.deepEqual(sent, [["tap", BACKSPACE], ["tap", BACKSPACE]]);
  }
});

test("deleting past the start of a composition reaches the host", () => {
  const { field, sent, placeholder } = fixture();
  fire(field, "compositionstart");
  edit(field, "insertCompositionText", { data: "a", cancelable: false, isComposing: true });
  edit(field, "deleteContentBackward", { cancelable: false, isComposing: true });
  assert.deepEqual(sent, []);
  edit(field, "deleteContentBackward", { cancelable: false, isComposing: true });
  assert.deepEqual(sent, [["tap", BACKSPACE]]);
  assert.equal(field.value, placeholder);
  fire(field, "compositionend", { data: "" });
  assert.deepEqual(sent, [["tap", BACKSPACE]]);
});

test("text dropped away from the caret is sent without the placeholder", () => {
  for (const position of [0, 1]) {
    for (const data of ["dropped", "a\u200b"]) {
      const { field, sent, placeholder } = fixture();
      // Drops report no data in beforeinput, and insert at the drop point.
      fire(field, "beforeinput", { inputType: "insertFromDrop", data: null });
      field.value = field.value.slice(0, position) + data + field.value.slice(position);
      fire(field, "input", { inputType: "insertFromDrop", data: null });
      assert.deepEqual(sent, [["text", data]], `at ${position}`);
      assert.equal(field.value, placeholder);
    }
  }
});

test("cutting the placeholder sends nothing and restores it", () => {
  const { field, sent, placeholder } = fixture();
  field.setSelectionRange(0, placeholder.length);
  edit(field, "deleteByCut", { cancelable: false });
  assert.deepEqual(sent, []);
  assert.equal(field.value, placeholder);
  assert.equal(field.selectionStart, placeholder.length);
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
