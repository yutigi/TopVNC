// One execution context per connection. Invalidate callbacks synchronously,
// then wait for release/close acknowledgement before allowing a replacement.
export class SessionSlot {
  constructor(onMessage, timeoutMs = 2000) {
    this.onMessage = onMessage;
    this.timeoutMs = timeoutMs;
    this.generation = 0;
    this.draining = Promise.resolve();
  }

  dispose(current) {
    if (!current.context || current.disposed) return;
    current.disposed = true;
    current.context.dispose();
  }

  async start(factory) {
    const generation = ++this.generation;
    await this.draining;
    if (generation !== this.generation) return;
    const current = { generation };
    this.current = current;
    const active = () => this.current === current && generation === this.generation;
    const emit = (message) => {
      if (current.closed) return;
      if (message.type === "closed") {
        current.closed = true;
        current.ack?.();
      }
      if (active()) this.onMessage(message, active);
      if (message.type === "closed") this.dispose(current);
    };
    const context = await factory(emit, active);
    current.context = context;
    if (!active() || current.closed) this.dispose(current);
    return active();
  }

  send(message) {
    if (!this.current?.closed) this.current?.context?.post(message);
  }

  stop() {
    ++this.generation;
    const current = this.current;
    this.current = undefined;
    if (!current) return this.draining;
    this.draining = this.draining.then(async () => {
      if (!current.context) return; // Async factory sees invalidation and disposes itself.
      if (!current.closed) {
        await new Promise((resolve) => {
          const timer = setTimeout(resolve, this.timeoutMs);
          current.ack = () => { clearTimeout(timer); resolve(); };
          try {
            current.context.post({ type: "close" }); // Session.close releases all held input.
          } catch {
            current.ack(); // A failed worker still gets disposed below.
          }
        });
      }
      this.dispose(current);
    });
    return this.draining;
  }
}
