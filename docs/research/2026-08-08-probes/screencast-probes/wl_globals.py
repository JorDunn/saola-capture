#!/usr/bin/env python3
"""Read-only Wayland registry dump: binds nothing, maps nothing.
Sends wl_display.get_registry + wl_display.sync, prints global events."""
import os, socket, struct, sys

xdg = os.environ["XDG_RUNTIME_DIR"]
disp = os.environ.get("WAYLAND_DISPLAY", "wayland-1")
path = disp if disp.startswith("/") else os.path.join(xdg, disp)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(path)
s.settimeout(3.0)

def msg(obj, opcode, payload=b""):
    return struct.pack("<II", obj, ((8 + len(payload)) << 16) | opcode) + payload

# wl_display(1).get_registry(new_id=2) ; wl_display(1).sync(new_id=3)
s.sendall(msg(1, 1, struct.pack("<I", 2)) + msg(1, 0, struct.pack("<I", 3)))

buf = b""
done = False
globals_ = []
while not done:
    try:
        chunk = s.recv(65536)
    except socket.timeout:
        break
    if not chunk:
        break
    buf += chunk
    while len(buf) >= 8:
        obj, so = struct.unpack_from("<II", buf)
        size, opcode = so >> 16, so & 0xFFFF
        if len(buf) < size:
            break
        payload = buf[8:size]
        buf = buf[size:]
        if obj == 2 and opcode == 0:  # wl_registry.global(name, interface, version)
            name, slen = struct.unpack_from("<II", payload)
            iface = payload[8 : 8 + slen - 1].decode()
            pad = (4 - (slen % 4)) % 4
            (ver,) = struct.unpack_from("<I", payload, 8 + slen + pad)
            globals_.append((name, iface, ver))
        elif obj == 3 and opcode == 0:  # wl_callback.done -> registry roundtrip complete
            done = True
s.close()
for name, iface, ver in sorted(globals_, key=lambda g: g[1]):
    print(f"{iface} v{ver} (name {name})")
