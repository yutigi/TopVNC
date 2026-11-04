export function makePayload(sequence) {
  if (!Number.isSafeInteger(sequence) || sequence < 0) throw new Error("Invalid sequence");
  const payload = Uint8Array.from({ length: 1024 }, (_, index) => (index + sequence) % 251);
  new DataView(payload.buffer).setBigUint64(0, BigInt(sequence));
  return payload;
}

// TCP/WebSocket message boundaries need not match the sent sample boundaries.
export function verifyChunk(expected, offset, chunk) {
  if (!(expected instanceof Uint8Array) || !(chunk instanceof Uint8Array)
      || !Number.isSafeInteger(offset) || offset < 0 || offset >= expected.length
      || chunk.length === 0 || chunk.length > expected.length - offset) {
    throw new Error("Unexpected echo length");
  }
  for (let index = 0; index < chunk.length; index += 1) {
    if (chunk[index] !== expected[offset + index]) throw new Error("Corrupt echo");
  }
  return offset + chunk.length;
}
