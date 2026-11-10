// Session settings and Customize are modal dialogs that share one stack. It
// alone decides what is inert beneath the open dialog, where Escape and Tab go
// and where focus returns, so closing one dialog cannot expose the page under
// another. Escape and Tab work wherever focus is, including on the page body
// after a click on a dialog's background.
const FOCUSABLE = "button, input, select, textarea";

export function createModals({ background, fallback }) {
  const stack = [];
  const targets = (dialog) => [...dialog.querySelectorAll(FOCUSABLE)].filter((node) => !node.disabled && node.getClientRects().length);
  const focus = (node) => node?.focus({ preventScroll: true });

  function sync() {
    for (const node of background) node.inert = stack.length > 0;
    stack.forEach((entry, index) => { entry.dialog.inert = index < stack.length - 1; });
  }

  // The opener is the element focus returns to; it defaults to the one
  // focused now, so callers that move focus first pass it explicitly.
  function open(dialog, { initial, opener = document.activeElement, closed } = {}) {
    if (stack.some((entry) => entry.dialog === dialog)) return;
    stack.push({ dialog, opener, closed });
    dialog.hidden = false;
    sync();
    focus(initial);
  }

  function close(dialog) {
    dialog.hidden = true;
    const index = stack.findIndex((entry) => entry.dialog === dialog);
    if (index < 0) return;
    const top = index === stack.length - 1;
    const [entry] = stack.splice(index, 1);
    dialog.inert = false;
    sync();
    entry.closed?.();
    if (!top) return;
    const below = stack.at(-1);
    if (below) focus(targets(below.dialog)[0]);
    else {
      const { opener } = entry;
      focus(opener && opener !== document.body && opener.getClientRects().length ? opener : fallback());
    }
  }

  document.addEventListener("keydown", (event) => {
    const entry = stack.at(-1);
    if (!entry || event.defaultPrevented) return;
    if (event.key === "Escape") {
      event.preventDefault();
      close(entry.dialog);
      return;
    }
    if (event.key !== "Tab") return;
    const nodes = targets(entry.dialog);
    const at = nodes.indexOf(document.activeElement);
    if (at < 0 || at === (event.shiftKey ? 0 : nodes.length - 1)) {
      event.preventDefault();
      focus(event.shiftKey ? nodes.at(-1) : nodes[0]);
    }
  });

  return { open, close, active: () => stack.length > 0 };
}
