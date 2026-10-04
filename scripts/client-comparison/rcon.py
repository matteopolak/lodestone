import socket, struct

class Rcon:
    def __init__(self, port=25581, password='lodestone', timeout=60):
        self.s = socket.create_connection(('127.0.0.1', port), timeout=timeout); self.n = 1
        self._send(3, password); i, _ = self._recv()
        if i != 1: raise RuntimeError('rcon auth failed')
    def _send(self, t, body):
        b = struct.pack('<ii', self.n, t) + body.encode() + b'\0\0'
        self.s.sendall(struct.pack('<i', len(b)) + b)
    def _recv(self):
        h = b''
        while len(h) < 4: h += self.s.recv(4 - len(h))
        (l,) = struct.unpack('<i', h); d = b''
        while len(d) < l: d += self.s.recv(l - len(d))
        i, t = struct.unpack('<ii', d[:8]); return i, d[8:-2].decode(errors='replace')
    def command(self, c):
        self.n += 1; self._send(2, c); i, p = self._recv(); return p.strip()
    def __enter__(self): return self
    def __exit__(self, *a): self.s.close()
