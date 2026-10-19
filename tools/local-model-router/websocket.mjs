import { createHash } from 'node:crypto';

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const DEFAULT_MAX_BYTES = 8 * 1024 * 1024;
const MAX_CONTROL_BYTES = 125;
const MAX_QUEUE_BYTES = 64 * 1024;

const hasToken = (value, token) =>
  typeof value === 'string' && value.split(',').some((part) => part.trim().toLowerCase() === token);

function closeCodeIsValid(code) {
  return (code >= 1000 && code <= 1014 && ![1004, 1005, 1006].includes(code)) || (code >= 3000 && code <= 4999);
}

function frame(opcode, payload = Buffer.alloc(0)) {
  if (payload.length < 126) return Buffer.concat([Buffer.from([0x80 | opcode, payload.length]), payload]);
  if (payload.length <= 0xffff) {
    const header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(payload.length, 2);
    return Buffer.concat([header, payload]);
  }
  const header = Buffer.alloc(10);
  header[0] = 0x80 | opcode;
  header[1] = 127;
  header.writeBigUInt64BE(BigInt(payload.length), 2);
  return Buffer.concat([header, payload]);
}

/**
 * Attach a deliberately small RFC 6455 server transport to a Node HTTP upgrade.
 * The caller owns HTTP routing and loopback enforcement. No extensions are negotiated.
 */
export function acceptWebSocket(request, socket, head, {
  onMessage,
  onClose,
  maxBytes = DEFAULT_MAX_BYTES,
} = {}) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new RangeError('maxBytes must be a positive safe integer');

  const headers = request?.headers ?? {};
  const key = headers['sec-websocket-key'];
  const validKey = typeof key === 'string' && /^[A-Za-z0-9+/]{22}==$/.test(key) && Buffer.from(key, 'base64').length === 16;
  if (request?.method !== 'GET' || !hasToken(headers.upgrade, 'websocket') || !hasToken(headers.connection, 'upgrade') ||
      headers['sec-websocket-version'] !== '13' || !validKey) {
    socket.write('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n');
    socket.destroy();
    throw new Error('invalid WebSocket handshake');
  }

  const accept = createHash('sha1').update(key + GUID).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);

  let closed = false;
  let closeSent = false;
  let closeReceived = false;
  let closeNotified = false;
  let input = Buffer.alloc(0);
  let outputQueued = 0;
  let fragmented = false;
  let messageBytes = 0;
  let messageParts = [];
  let decoder;
  let closeTimer;

  const notifyClose = () => {
    if (closeNotified) return;
    clearTimeout(closeTimer);
    closeNotified = true;
    onClose?.();
  };

  const terminate = () => {
    if (closed) return;
    closed = true;
    socket.destroy();
    notifyClose();
  };

  const write = (data) => {
    if (closed) return false;
    if (outputQueued + data.length > maxBytes + MAX_QUEUE_BYTES) {
      terminate();
      return false;
    }
    outputQueued += data.length;
    socket.write(data, () => {
      outputQueued -= data.length;
    });
    return true;
  };

  const sendClose = (code, reason = '') => {
    if (closeSent || closed) return;
    const reasonBytes = Buffer.from(String(reason), 'utf8');
    if (!closeCodeIsValid(code) || reasonBytes.length > 123) throw new RangeError('invalid close code or reason');
    const payload = Buffer.concat([Buffer.from([code >> 8, code & 0xff]), reasonBytes]);
    closeSent = true;
    write(frame(0x8, payload));
    closeTimer = setTimeout(terminate, 500);
    closeTimer.unref();
  };

  const protocolError = (code = 1002) => {
    if (!closeSent && !closed) {
      closeSent = true;
      write(frame(0x8, Buffer.from([code >> 8, code & 0xff])));
    }
    socket.end();
    closeTimer ??= setTimeout(terminate, 500);
    closeTimer.unref();
  };

  const processFrame = () => {
    while (!closed) {
      if (input.length < 2) return;
      const first = input[0];
      const second = input[1];
      const fin = (first & 0x80) !== 0;
      const rsv = first & 0x70;
      const opcode = first & 0x0f;
      const masked = (second & 0x80) !== 0;
      let length = second & 0x7f;
      let offset = 2;
      if (rsv || !masked || (opcode >= 0x8 && (!fin || length > MAX_CONTROL_BYTES))) return protocolError();
      if (length === 126) {
        if (input.length < 4) return;
        length = input.readUInt16BE(2);
        if (length < 126) return protocolError();
        offset = 4;
      } else if (length === 127) {
        if (input.length < 10) return;
        const wide = input.readBigUInt64BE(2);
        if (wide > BigInt(Number.MAX_SAFE_INTEGER)) return protocolError(1009);
        length = Number(wide);
        if (length <= 0xffff) return protocolError();
        offset = 10;
      }
      if (length > maxBytes || input.length < offset + 4 + length) {
        if (length > maxBytes) return protocolError(1009);
        return;
      }
      const mask = input.subarray(offset, offset + 4);
      const payload = Buffer.allocUnsafe(length);
      for (let i = 0; i < length; i++) payload[i] = input[offset + 4 + i] ^ mask[i & 3];
      input = input.subarray(offset + 4 + length);

      if (opcode === 0x8) {
        if (length === 1) return protocolError();
        if (length >= 2) {
          const code = payload.readUInt16BE(0);
          if (!closeCodeIsValid(code)) return protocolError();
          try { new TextDecoder('utf-8', { fatal: true }).decode(payload.subarray(2)); } catch { return protocolError(1007); }
        }
        closeReceived = true;
        if (!closeSent) { closeSent = true; write(frame(0x8, payload)); }
        socket.end();
        continue;
      }
      if (opcode === 0x9) { write(frame(0xa, payload)); continue; }
      if (opcode === 0xa) continue;
      if (closeSent) continue;
      if (opcode === 0x2 || opcode > 0x2) return protocolError();
      if (opcode === 0x1) {
        if (fragmented) return protocolError();
        fragmented = !fin;
        messageBytes = length;
        messageParts = [payload];
        decoder = new TextDecoder('utf-8', { fatal: true });
        try { decoder.decode(payload, { stream: !fin }); } catch { return protocolError(1007); }
      } else if (opcode === 0x0) {
        if (!fragmented) return protocolError();
        messageBytes += length;
        if (messageBytes > maxBytes) return protocolError(1009);
        messageParts.push(payload);
        if (messageParts.length > 4096) return protocolError(1009);
        try { decoder.decode(payload, { stream: !fin }); } catch { return protocolError(1007); }
        fragmented = !fin;
      }
      if ((opcode === 0x1 || opcode === 0x0) && fin) {
        try { decoder.decode(); } catch { return protocolError(1007); }
        onMessage?.(Buffer.concat(messageParts).toString('utf8'));
        messageParts = [];
        messageBytes = 0;
        decoder = undefined;
      }
    }
  };

  const receive = (chunk) => {
    if (closed || closeReceived) return;
    if (input.length + chunk.length > maxBytes + MAX_QUEUE_BYTES) return protocolError(1009);
    input = input.length === 0 ? chunk : Buffer.concat([input, chunk]);
    processFrame();
  };

  socket.on('data', receive);
  socket.on('error', terminate);
  socket.on('end', terminate);
  socket.on('close', () => { closed = true; notifyClose(); });
  if (head?.length) receive(head);

  return {
    send(text) {
      if (closed || closeSent) return false;
      const payload = Buffer.from(String(text), 'utf8');
      if (payload.length > maxBytes) throw new RangeError('message exceeds maxBytes');
      return write(frame(0x1, payload));
    },
    close(code = 1000, reason = '') {
      if (closed) return;
      sendClose(code, reason);
      if (closeReceived) socket.end();
    },
    get closed() { return closed; },
  };
}
