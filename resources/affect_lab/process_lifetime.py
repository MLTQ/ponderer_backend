"""Portable parent ownership and native process-group supervision."""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time


def watch_parent(parent, on_exit):
    if parent <= 1 or os.getppid() != parent:
        raise RuntimeError("Owning process exited before startup")

    def watch():
        while os.getppid() == parent:
            time.sleep(0.1)
        on_exit()

    threading.Thread(target=watch, name="local-parent-watch", daemon=True).start()


def native_child_command(arguments):
    return [sys.executable, str(Path(__file__).resolve()), str(os.getpid()), "--", *arguments]


def exec_child(arguments):
    parent = int(arguments[0])
    command = arguments[2:]
    if not command or arguments[1] != "--":
        raise ValueError("Missing child executable")
    native = None

    def kill_group():
        if native is not None:
            try:
                os.killpg(native.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def terminate_group(signum, _frame):
        kill_group()
        raise SystemExit(128 + signum)

    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    signal.signal(signal.SIGTERM, terminate_group)
    signal.signal(signal.SIGINT, terminate_group)
    if sys.platform.startswith("linux"):
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "Cannot establish child parent-death safeguard")
    if os.getppid() != parent:
        raise RuntimeError("Owning process exited before child startup")
    native = subprocess.Popen(command, start_new_session=True)
    try:
        watch_parent(parent, lambda: os.kill(os.getpid(), signal.SIGTERM))
        signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM, signal.SIGINT})
        code = native.wait()
    finally:
        # Reap outside the signal handler: Popen.wait's lock is not reentrant.
        kill_group()
        native.wait(timeout=3)
    raise SystemExit(code)


if __name__ == "__main__":
    exec_child(sys.argv[1:])
