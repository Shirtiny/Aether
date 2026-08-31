#!/usr/bin/env python3
import concurrent.futures
import contextlib
import socket
import socketserver
import struct
import subprocess
import sys
import threading
import time


class EchoHandler(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(65536)
            if not data:
                return
            self.request.sendall(data)


class ThreadingTCPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


def unused_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def recv_exact(sock, count):
    result = bytearray()
    while len(result) < count:
        chunk = sock.recv(count - len(result))
        if not chunk:
            raise RuntimeError(f"unexpected EOF after {len(result)}/{count} bytes")
        result.extend(chunk)
    return bytes(result)


def send_fragmented(sock, payload, delay=0.0005, chunks=None):
    if chunks is None:
        chunks = [bytes([value]) for value in payload]
    for chunk in chunks:
        sock.sendall(chunk)
        if delay:
            time.sleep(delay)


@contextlib.contextmanager
def microsocks(binary, port, auth=None):
    command = [binary, "-q", "-i", "127.0.0.1", "-p", str(port)]
    if auth:
        command.extend(["-u", auth[0], "-P", auth[1]])
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while True:
            if process.poll() is not None:
                raise RuntimeError(process.stderr.read().decode(errors="replace"))
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.01)
        yield
    finally:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def greeting_result(proxy_port, delay, split=True):
    with socket.create_connection(("127.0.0.1", proxy_port), timeout=2) as sock:
        sock.settimeout(2)
        if split:
            send_fragmented(sock, b"\x05\x01\x00", delay, [b"\x05\x01", b"\x00"])
        else:
            sock.sendall(b"\x05\x01\x00")
        return recv_exact(sock, 2)


def proxy_echo(proxy_port, target_port, address_type, auth=None, delay=0.0002):
    with socket.create_connection(("127.0.0.1", proxy_port), timeout=3) as sock:
        sock.settimeout(3)
        methods = b"\x00" if auth is None else b"\x00\x02"
        send_fragmented(sock, bytes([5, len(methods)]) + methods, delay)
        expected_method = 0 if auth is None else 2
        choice = recv_exact(sock, 2)
        if choice != bytes([5, expected_method]):
            raise AssertionError(f"method choice {choice.hex()} != 05{expected_method:02x}")

        if auth is not None:
            username, password = (value.encode() for value in auth)
            credentials = bytes([1, len(username)]) + username + bytes([len(password)]) + password
            send_fragmented(sock, credentials, delay)
            response = recv_exact(sock, 2)
            if response != b"\x01\x00":
                raise AssertionError(f"auth response {response.hex()} != 0100")

        port = struct.pack("!H", target_port)
        if address_type == "ipv4":
            request = b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + port
        elif address_type == "domain":
            domain = b"127.0.0.1"
            request = b"\x05\x01\x00\x03" + bytes([len(domain)]) + domain + port
        else:
            raise ValueError(address_type)
        send_fragmented(sock, request, delay)
        response = recv_exact(sock, 10)
        if response[:2] != b"\x05\x00":
            raise AssertionError(f"connect response {response.hex()}")

        payload = f"framing-{address_type}-{threading.get_ident()}".encode()
        sock.sendall(payload)
        echoed = recv_exact(sock, len(payload))
        if echoed != payload:
            raise AssertionError("proxied echo mismatch")


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} ORIGINAL PATCHED")
    original, patched = sys.argv[1:]

    echo = ThreadingTCPServer(("127.0.0.1", 0), EchoHandler)
    echo_thread = threading.Thread(target=echo.serve_forever, daemon=True)
    echo_thread.start()
    target_port = echo.server_address[1]

    try:
        original_port = unused_port()
        with microsocks(original, original_port):
            original_split = greeting_result(original_port, 0.02, split=True)
            original_combined = greeting_result(original_port, 0, split=False)
        if original_split != b"\x05\xff" or original_combined != b"\x05\x00":
            raise AssertionError(
                f"original reproduction changed: split={original_split.hex()} combined={original_combined.hex()}"
            )

        patched_port = unused_port()
        with microsocks(patched, patched_port):
            matrix = {}
            for delay in (0, 0.0001, 0.001, 0.005, 0.02):
                outcomes = [greeting_result(patched_port, delay, split=True) for _ in range(50)]
                matrix[delay] = {outcome.hex(): outcomes.count(outcome) for outcome in set(outcomes)}
                if any(outcome != b"\x05\x00" for outcome in outcomes):
                    raise AssertionError(f"patched greeting matrix failed at {delay}: {matrix[delay]}")

            for address_type in ("ipv4", "domain"):
                for _ in range(25):
                    proxy_echo(patched_port, target_port, address_type)

            jobs = []
            with concurrent.futures.ThreadPoolExecutor(max_workers=64) as executor:
                for index in range(500):
                    jobs.append(
                        executor.submit(
                            proxy_echo,
                            patched_port,
                            target_port,
                            "ipv4" if index % 2 == 0 else "domain",
                            None,
                            0.0001,
                        )
                    )
                for job in jobs:
                    job.result()

        auth = ("aether-test-user", "aether-test-password")
        auth_port = unused_port()
        with microsocks(patched, auth_port, auth=auth):
            for address_type in ("ipv4", "domain"):
                for _ in range(20):
                    proxy_echo(auth_port, target_port, address_type, auth=auth)

        print(f"original split={original_split.hex()} combined={original_combined.hex()}")
        print(f"patched greeting matrix={matrix}")
        print("patched fragmented CONNECT: 50 sequential + 500 concurrent passed")
        print("patched fragmented username/password auth: 40 passed")
        print("RESULT=PASS")
    finally:
        echo.shutdown()
        echo.server_close()


if __name__ == "__main__":
    main()
