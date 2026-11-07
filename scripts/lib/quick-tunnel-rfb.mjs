// A bounded, loopback-only RFB 3.8 fixture. Input is recorded in memory and
// never injected into the OS. Authentication is mandatory, using a disposable
// eight-byte password supplied by the live-check harness.
import { createServer } from "node:net";
import { createCipheriv, randomBytes, timingSafeEqual } from "node:crypto";

export function challengeResponse(password, challenge) {
  const key = Buffer.alloc(8);
  Buffer.from(password).copy(key, 0, 0, 8);
  for (let i = 0; i < 8; i++) {
    let reversed = 0;
    for (let bit = 0; bit < 8; bit++) reversed |= ((key[i] >> bit) & 1) << (7 - bit);
    key[i] = reversed;
  }
  // EDE with three identical keys is DES; Node's maintained OpenSSL providers
  // expose 3DES even when standalone legacy DES is unavailable.
  const cipher = createCipheriv("des-ede3", Buffer.concat([key, key, key]), null);
  cipher.setAutoPadding(false);
  return Buffer.concat([cipher.update(challenge), cipher.final()]);
}

export async function startRfb(password) {
  const width = 320, height = 180;
  const sockets = new Set();
  const audit = { connections: 0, authenticated: 0, rejected: 0, frames: 0, keyDown: 0, keyUp: 0,
    pointerDown: 0, pointerUp: 0, heldKeys: new Set(), buttons: 0 };
  const server = createServer(socket => {
    if (sockets.size >= 8) { socket.destroy(); return; }
    audit.connections++;
    sockets.add(socket);
    let buffer = Buffer.alloc(0), stage = "version", timer, frame = 0;
    const heldKeys = new Set();
    const challenge = randomBytes(16);
    const sendFrame = () => {
      timer = null;
      if (socket.destroyed || socket.writableLength > 1_048_576) return;
      const update = Buffer.alloc(16 + width * height * 4);
      update.writeUInt16BE(1, 2);
      update.writeUInt16BE(width, 8);
      update.writeUInt16BE(height, 10);
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          const at = 16 + (y * width + x) * 4;
          update[at] = (x + frame * 13) % 256;
          update[at + 1] = y;
          update[at + 2] = frame % 2 ? 180 : 60;
        }
      }
      frame++;
      audit.frames++;
      socket.write(update);
    };
    socket.on("error", () => {});
    socket.setTimeout(10_000, () => socket.destroy());
    socket.on("close", () => {
      clearTimeout(timer);
      sockets.delete(socket);
      for (const key of heldKeys) audit.heldKeys.delete(key);
      audit.buttons = 0;
    });
    socket.write("RFB 003.008\n");
    socket.on("data", chunk => {
      if (buffer.length + chunk.length > 1_048_576) return socket.destroy();
      buffer = Buffer.concat([buffer, chunk]);
      const take = n => { const part = buffer.subarray(0, n); buffer = buffer.subarray(n); return part; };
      while (!socket.destroyed) {
        if (stage === "version") {
          if (buffer.length < 12) break;
          if (take(12).toString() !== "RFB 003.008\n") return socket.destroy();
          socket.write(Buffer.from([1, 2]));
          stage = "security";
        } else if (stage === "security") {
          if (buffer.length < 1) break;
          if (take(1)[0] !== 2) return socket.destroy();
          socket.write(challenge);
          stage = "response";
        } else if (stage === "response") {
          if (buffer.length < 16) break;
          if (!timingSafeEqual(take(16), challengeResponse(password, challenge))) {
            audit.rejected++;
            socket.end(Buffer.from([0, 0, 0, 1, 0, 0, 0, 0]));
            return;
          }
          socket.write(Buffer.alloc(4));
          socket.setTimeout(0);
          audit.authenticated++;
          stage = "init";
        } else if (stage === "init") {
          if (buffer.length < 1) break;
          take(1);
          const name = Buffer.from("TopVNC disposable test pattern");
          const init = Buffer.alloc(24);
          init.writeUInt16BE(width, 0); init.writeUInt16BE(height, 2);
          Buffer.from([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]).copy(init, 4);
          init.writeUInt32BE(name.length, 20);
          socket.write(Buffer.concat([init, name]));
          stage = "messages";
        } else {
          if (!buffer.length) break;
          const type = buffer[0];
          let size;
          switch (type) {
            case 0: size = 20; break;
            case 2:
              if (buffer.length < 4) return;
              if (buffer.readUInt16BE(2) > 256) return socket.destroy();
              size = 4 + buffer.readUInt16BE(2) * 4; break;
            case 3: size = 10; break;
            case 4: size = 8; break;
            case 5: size = 6; break;
            case 6:
              if (buffer.length < 8) return;
              if (buffer.readUInt32BE(4) > 65_536) return socket.destroy();
              size = 8 + buffer.readUInt32BE(4); break;
            case 248: // Fence: the browser sends bounded RTT probes.
              if (buffer.length < 9) return;
              if (buffer[8] > 64) return socket.destroy();
              size = 9 + buffer[8]; break;
            default: audit.unsupportedMessage = type; return socket.destroy();
          }
          if (buffer.length < size) break;
          const message = take(size);
          if (type === 0 && !message.subarray(4).equals(
            Buffer.from([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]))) {
            audit.unsupportedPixelFormat = true; return socket.destroy();
          }
          if (type === 3 && !timer) timer = setTimeout(sendFrame, 200);
          if (type === 4) {
            const key = message.readUInt32BE(4);
            if (message[1] && !heldKeys.has(key) && heldKeys.size >= 256) return socket.destroy();
            if (message[1]) { heldKeys.add(key); audit.heldKeys.add(key); audit.keyDown++; }
            else { heldKeys.delete(key); audit.heldKeys.delete(key); audit.keyUp++; }
          }
          if (type === 5) {
            audit.buttons = message[1];
            if (message[1]) audit.pointerDown++; else audit.pointerUp++;
          }
          if (type === 248 && (message.readUInt32BE(4) & 0x8000_0000)) {
            const reply = Buffer.from(message);
            reply.writeUInt32BE(message.readUInt32BE(4) & 7, 4);
            socket.write(reply);
          }
        }
      }
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return { address: `127.0.0.1:${server.address().port}`, width, height, audit,
    async close() {
      for (const socket of sockets) socket.destroy();
      await new Promise(resolve => server.close(resolve));
    } };
}
