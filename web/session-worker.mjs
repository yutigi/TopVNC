// Dedicated Worker entry: the session runs here, drawing to the
// OffscreenCanvas the page transferred.
import { createSession } from "./session.mjs";

const onMessage = createSession((message) => self.postMessage(message));
self.onmessage = (event) => onMessage(event.data);
