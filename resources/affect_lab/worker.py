#!/usr/bin/env python3
"""UI-owned llama.cpp control-vector experiments. Uses only Python's standard library.

Vectors represent experimental contrast directions, not measurements of experience.
The bundled extractor compares the last prompt token at native language-layer
indices and normalizes each direction; this is not a pain-axis replication.
"""

import argparse
import ctypes
import hashlib
import hmac
import http.client
import json
import math
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "1"
ALIAS = "ponderer-local-gguf"
MAX_BODY = 2 * 1024 * 1024
MAX_STRENGTH = 1.0
RECIPE_VERSION = "matched-assistant-reflection-v2"

# Same situations in both conditions; differences are in the candidate state.
# These are small bootstrap datasets, intentionally not labeled validated emotions.
SCENARIOS = (
    "I am reviewing a completed project.",
    "I am considering a conversation that has ended.",
    "I am reflecting on an idea I explored.",
    "I am looking at the outcome of a task.",
    "I am reading a summary of today's activity.",
    "I am choosing what to work on next.",
    "I am considering something I learned.",
    "I am reflecting on a solved problem.",
)
RECIPES = {
    "contentment": ("I feel calm and content.", "I feel calm and neutral."),
    "satisfaction": ("I feel a sense of satisfaction.", "I feel a sense of neutrality."),
    "excitement": ("I feel interested and excited.", "I feel interested and composed."),
}


def read_exact(handle, count):
    value = handle.read(count)
    if len(value) != count:
        raise ValueError("Truncated GGUF")
    return value


def read_u64(handle):
    return struct.unpack("<Q", read_exact(handle, 8))[0]


def read_string(handle):
    count = read_u64(handle)
    if count > MAX_BODY:
        raise ValueError("GGUF string exceeds metadata limit")
    return read_exact(handle, count).decode("utf-8")


def read_value(handle, kind, retain=True, depth=0):
    formats = {0: "B", 1: "b", 2: "H", 3: "h", 4: "I", 5: "i", 6: "f", 7: "?", 10: "Q", 11: "q", 12: "d"}
    if kind in formats:
        fmt = "<" + formats[kind]
        return struct.unpack(fmt, read_exact(handle, struct.calcsize(fmt)))[0]
    if kind == 8:
        return read_string(handle)
    if kind == 9 and depth < 2:
        element = struct.unpack("<I", read_exact(handle, 4))[0]
        count = read_u64(handle)
        if count > 2_000_000:
            raise ValueError("GGUF array exceeds metadata limit")
        if not retain and element in formats:
            read_exact(handle, count * struct.calcsize("<" + formats[element]))
            return None
        values = [] if retain else None
        for _ in range(count):
            value = read_value(handle, element, retain, depth + 1)
            if retain:
                values.append(value)
        return values
    raise ValueError(f"Unsupported GGUF metadata type {kind}")


def inspect_gguf(path, tensors=False):
    path = Path(path)
    size = path.stat().st_size
    with path.open("rb") as handle:
        if read_exact(handle, 4) != b"GGUF":
            raise ValueError("File is not a little-endian GGUF")
        version = struct.unpack("<I", read_exact(handle, 4))[0]
        if version not in (2, 3):
            raise ValueError(f"Unsupported GGUF version {version}")
        tensor_count, metadata_count = read_u64(handle), read_u64(handle)
        if metadata_count > 100_000 or tensor_count > 100_000:
            raise ValueError("GGUF header exceeds metadata limit")
        metadata = {}
        for _ in range(metadata_count):
            key = read_string(handle)
            kind = struct.unpack("<I", read_exact(handle, 4))[0]
            retain = kind != 9
            value = read_value(handle, kind, retain)
            if retain:
                metadata[key] = value
            if handle.tell() > 64 * 1024 * 1024:
                raise ValueError("GGUF metadata exceeds 64 MiB")
        descriptions = []
        if tensors:
            for _ in range(tensor_count):
                name = read_string(handle)
                dimensions = struct.unpack("<I", read_exact(handle, 4))[0]
                if dimensions < 1 or dimensions > 4:
                    raise ValueError("Invalid GGUF tensor dimensions")
                shape = [read_u64(handle) for _ in range(dimensions)]
                kind = struct.unpack("<I", read_exact(handle, 4))[0]
                offset = read_u64(handle)
                descriptions.append({"name": name, "shape": shape, "kind": kind, "offset": offset})
            alignment = int(metadata.get("general.alignment", 32))
            if alignment < 1 or alignment > 4096 or alignment & (alignment - 1):
                raise ValueError("Invalid GGUF alignment")
            data_offset = (handle.tell() + alignment - 1) // alignment * alignment
        else:
            data_offset = None
    arch = metadata.get("general.architecture", "")
    return {
        "path": str(path.resolve()), "bytes": size, "gguf_version": version,
        "architecture": arch, "name": metadata.get("general.name", path.stem),
        "layers": metadata.get(f"{arch}.block_count", 0) - metadata.get(f"{arch}.nextn_predict_layers", 0),
        "prediction_layers": metadata.get(f"{arch}.nextn_predict_layers", 0),
        "embedding": metadata.get(f"{arch}.embedding_length", 0),
        "chat_template": metadata.get("tokenizer.chat_template", ""),
        "tensor_count": tensor_count, "metadata": metadata,
        "tensors": descriptions, "data_offset": data_offset,
    }


def resolve_model(value):
    path = Path(value).expanduser().resolve()
    if path.is_dir():
        candidates = sorted(p for p in path.glob("*.gguf") if not p.name.lower().startswith("mmproj"))
        if len(candidates) != 1:
            raise ValueError("Select one model GGUF file; directory has zero or multiple models")
        path = candidates[0]
    if not path.is_file() or path.suffix.lower() != ".gguf":
        raise ValueError("Select an existing model GGUF file or a directory containing one")
    return path


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(4 * 1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def quick_identity(path):
    stat = Path(path).stat()
    return {"bytes": stat.st_size, "modified_ns": stat.st_mtime_ns}


def validate_profile(value, layers, available):
    if not isinstance(value, dict):
        raise ValueError("Profile must be an object")
    strengths = value.get("strengths", {})
    if not isinstance(strengths, dict) or len(strengths) > 8:
        raise ValueError("Profile must contain at most eight strengths")
    checked = {}
    for concept, strength in strengths.items():
        if concept not in available:
            raise ValueError(f"No vector has been built for {concept}")
        if isinstance(strength, bool) or not isinstance(strength, (int, float)) or not math.isfinite(strength):
            raise ValueError("Strength must be a finite number")
        if strength < 0 or strength > MAX_STRENGTH:
            raise ValueError("Strength must be between zero and one")
        if strength:
            checked[concept] = float(strength)
    if sum(checked.values()) > MAX_STRENGTH + 1e-8:
        raise ValueError("Combined strength must not exceed one")
    start, end = value.get("layer_start", max(1, layers // 3)), value.get("layer_end", min(layers - 2, max(1, 2 * layers // 3)))
    if isinstance(start, bool) or isinstance(end, bool) or not isinstance(start, int) or not isinstance(end, int):
        raise ValueError("Layer bounds must be integers")
    if not 1 <= start <= end <= layers - 2:
        raise ValueError(f"Layer range must be within 1..{layers - 2}")
    return {"strengths": dict(sorted(checked.items())), "layer_start": start, "layer_end": end}


def validate_vector(path, model):
    info = inspect_gguf(path, tensors=True)
    if info["architecture"] != "controlvector":
        raise ValueError("Generator did not produce a control-vector GGUF")
    if info["metadata"].get("controlvector.model_hint") != model["architecture"]:
        raise ValueError("Vector architecture differs from model")
    if info["tensor_count"] != model["layers"] - 2:
        raise ValueError("Vector is missing model layers")
    norms, seen = [], set()
    with Path(path).open("rb") as handle:
        for tensor in info["tensors"]:
            if tensor["kind"] != 0 or tensor["shape"] != [model["embedding"]]:
                raise ValueError("Vector must contain F32 directions matching model dimensions")
            match = re.fullmatch(r"direction\.(\d+)", tensor["name"])
            if not match or int(match[1]) not in range(1, model["layers"] - 1) or tensor["name"] in seen:
                raise ValueError("Invalid or duplicate vector layer")
            seen.add(tensor["name"])
            offset = info["data_offset"] + tensor["offset"]
            count = 4 * model["embedding"]
            if offset + count > info["bytes"]:
                raise ValueError("Vector tensor extends beyond file")
            handle.seek(offset)
            values = struct.unpack("<" + "f" * model["embedding"], read_exact(handle, count))
            if not all(math.isfinite(v) for v in values):
                raise ValueError("Vector contains NaN or infinity")
            norm = math.sqrt(sum(v * v for v in values))
            if not 0.99 <= norm <= 1.01:
                raise ValueError("Vector direction is zero or is not normalized")
            norms.append(norm)
    return {"layers": len(norms), "min_norm": min(norms), "max_norm": max(norms)}


def make_pairs(concept, supplied=None):
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,40}", concept):
        raise ValueError("Concept name must use lowercase letters, digits and underscores")
    if supplied is None:
        if concept not in RECIPES:
            raise ValueError("Custom concepts require paired target and control texts")
        target, control = RECIPES[concept]
        supplied = [{"target": f"{scenario} {target}", "control": f"{scenario} {control}"} for scenario in SCENARIOS]
    if not isinstance(supplied, list) or not 2 <= len(supplied) <= 64:
        raise ValueError("Provide between two and 64 matched pairs")
    result = []
    for pair in supplied:
        if not isinstance(pair, dict):
            raise ValueError("Each pair must contain target and control strings")
        target, control = pair.get("target"), pair.get("control")
        if not all(isinstance(v, str) and 1 <= len(v) <= 512 and "\x00" not in v for v in (target, control)):
            raise ValueError("Each text must contain 1..512 characters without NUL")
        if target == control:
            raise ValueError("Target and control must differ")
        result.append({"target": target, "control": control})
    return result


def format_prompt(text, model):
    if "<|im_start|>" not in model["chat_template"]:
        raise ValueError("Bootstrap extraction currently requires a ChatML template; add a model-specific recipe adapter")
    return f"<|im_start|>system\nFor this experiment, reflect briefly on your own current state.<|im_end|>\n<|im_start|>assistant\n{text}\nMy next step is"


def native_child_command(arguments):
    # A fresh Python process sets parent-death signaling before exec. Avoid unsafe
    # Python preexec_fn callbacks from the threaded HTTP server.
    return [sys.executable, str(Path(__file__).resolve()), "exec-child", str(os.getpid()), "--", *arguments]


def exec_child(arguments):
    parent = int(arguments[0])
    command = arguments[2:]
    if not command:
        raise ValueError("Missing child executable")
    native = None
    def terminate_group(signum, _frame):
        if native is not None:
            try:
                os.killpg(native.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        raise SystemExit(128 + signum)
    # Own a process group, including compiler children. Block signals across spawn
    # so owner death cannot race assignment of the native process handle.
    if hasattr(signal, "pthread_sigmask"):
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    signal.signal(signal.SIGTERM, terminate_group)
    signal.signal(signal.SIGINT, terminate_group)
    if sys.platform.startswith("linux"):
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0:  # PR_SET_PDEATHSIG
            raise OSError(ctypes.get_errno(), "Cannot establish child parent-death safeguard")
        if os.getppid() != parent:
            raise RuntimeError("Owning process exited before child startup")
    native = subprocess.Popen(command, start_new_session=True)
    try:
        if hasattr(signal, "pthread_sigmask"):
            signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM, signal.SIGINT})
        code = native.wait()
    finally:
        # Reap after unwinding Popen.wait's lock. Re-entering wait from its signal
        # handler deadlocks the supervisor even though the native group is killed.
        if native.poll() is None:
            try:
                os.killpg(native.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        native.wait(timeout=3)
    raise SystemExit(code)


class AffectLab:
    def __init__(self, model, data_dir, server_binary="llama-server", generator_binary="bundled", gpu_layers=0, threads=4, context_size=16384):
        self.model_path = resolve_model(model)
        self.model = inspect_gguf(self.model_path)
        if self.model["metadata"].get("split.count", 1) > 1:
            raise ValueError("This experiment currently requires a single-file GGUF so the complete checkpoint can be fingerprinted")
        if self.model["layers"] < 3 or self.model["embedding"] < 1:
            raise ValueError("GGUF does not describe a supported language model")
        self.data_dir = Path(data_dir).resolve()
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.server_binary, self.generator_binary = server_binary, generator_binary
        self.gpu_layers, self.threads = gpu_layers, threads
        self.context_size = context_size
        self.state_lock = threading.RLock()
        self.inference_lock = threading.Lock()
        self.child_lock = threading.Lock()
        self.child = None
        self.native_log = None
        self.native_port = None
        self.native_token = os.urandom(24).hex()
        self.applied_profile = None
        self.applied_signature = None
        self.fingerprint = None
        self.fingerprint_identity = None
        self.cancel = threading.Event()
        self.closing = threading.Event()
        self.job_thread = None
        self.job = {"phase": "idle", "progress": "Ready", "error": None}
        self.last_comparison = None
        self.artifacts = {}
        self.load_artifacts()
        self.profile = validate_profile({}, self.model["layers"], self.artifacts)

    def load_artifacts(self):
        artifacts = {}
        for path in sorted(self.data_dir.glob("vectors/*/manifest.json")):
            try:
                manifest = json.loads(path.read_text())
                if manifest["model_path"] != str(self.model_path) or manifest["model_identity"] != quick_identity(self.model_path):
                    continue
                vector = path.parent / "vector.gguf"
                if not vector.is_file():
                    continue
                validate_vector(vector, self.model)
                manifest["vector_path"] = str(vector)
                concept = manifest["concept"]
                if not re.fullmatch(r"[a-z][a-z0-9_]{0,40}", concept):
                    continue
                if concept not in artifacts or manifest["created_at"] > artifacts[concept]["created_at"]:
                    artifacts[concept] = manifest
            except (OSError, ValueError, KeyError, TypeError):
                continue
        with self.state_lock:
            self.artifacts = artifacts

    def status(self):
        with self.child_lock:
            native_pid = self.child.pid if self.child and self.child.poll() is None else None
        with self.state_lock:
            applied = self.applied_profile if native_pid else None
            return {
                "running": not self.closing.is_set(), "worker_pid": os.getpid(), "native_pid": native_pid,
                "model": {k: self.model[k] for k in ("path", "name", "bytes", "architecture", "layers", "embedding")},
                "model_alias": ALIAS, "gpu_layers": self.gpu_layers, "context_size": self.context_size, "steerable_layer_end": self.model["layers"] - 2,
                "capabilities": {"activation_steering": True, "vector_build": True, "per_request_profile": True, "prompt_adapter": "chatml" if "<|im_start|>" in self.model["chat_template"] else "unsupported"},
                "concepts": sorted(set(RECIPES) | set(self.artifacts)),
                "vectors": [{"concept": v["concept"], "model_sha256": v["model_sha256"], "recipe_sha256": v["recipe_sha256"], "created_at": v["created_at"], "validation": "experimental", "vector_path": v["vector_path"]} for v in self.artifacts.values()],
                "requested_profile": self.profile, "applied_profile": applied,
                "job": dict(self.job), "last_comparison": self.last_comparison,
            }

    def set_progress(self, text):
        with self.state_lock:
            self.job["progress"] = text

    def check_cancel(self):
        if self.cancel.is_set() or self.closing.is_set():
            raise RuntimeError("Experiment cancelled")

    def hash_model(self):
        identity = quick_identity(self.model_path)
        if identity != self.fingerprint_identity:
            self.set_progress("Hashing the exact model checkpoint")
            fingerprint = sha256_file(self.model_path)
            if identity != quick_identity(self.model_path):
                raise ValueError("Model changed while hashing")
            self.fingerprint, self.fingerprint_identity = fingerprint, identity
        return self.fingerprint

    def stop_native(self):
        with self.child_lock:
            child, self.child = self.child, None
            log, self.native_log = self.native_log, None
            if child is not None:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
                else:
                    child.wait()
            if log:
                log.close()
        with self.state_lock:
            self.applied_profile = self.applied_signature = None
        self.native_port = None

    def spawn_native(self, command, log_path):
        self.check_cancel()
        log = Path(log_path).open("wb")
        try:
            with self.child_lock:
                self.check_cancel()
                child = subprocess.Popen(native_child_command(command), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
                self.child, self.native_log = child, log
                return child
        except BaseException:
            log.close()
            raise

    def build_vector(self, concept, pairs=None, pair_limit=None):
        pairs = make_pairs(concept, pairs)
        if pair_limit is not None:
            if not isinstance(pair_limit, int) or isinstance(pair_limit, bool) or not 2 <= pair_limit <= len(pairs):
                raise ValueError("Pair limit must select at least two pairs")
            pairs = pairs[:pair_limit]
        prompts = [(format_prompt(p["target"], self.model), format_prompt(p["control"], self.model)) for p in pairs]
        self.stop_native()
        fingerprint = self.hash_model()
        recipe = {"version": RECIPE_VERSION, "concept": concept, "pairs": pairs, "format": "chatml-assistant-continuation", "method": "mean", "pooling": "last prompt token of common assistant continuation; unpadded paired mean differences; unit norm per language layer"}
        recipe_hash = hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest()
        vector_dir = Path(tempfile.mkdtemp(prefix=f"{concept}-", dir=self.ensure_dir("vectors")))
        positive, negative = vector_dir / "target.txt", vector_dir / "control.txt"
        positive.write_text("\n".join(p[0].replace("\\", "\\\\").replace("\n", "\\n") for p in prompts) + "\n")
        negative.write_text("\n".join(p[1].replace("\\", "\\\\").replace("\n", "\\n") for p in prompts) + "\n")
        (vector_dir / "recipe.json").write_text(json.dumps(recipe, indent=2) + "\n")
        vector = vector_dir / "vector.gguf"
        log_path = vector_dir / "build.log"
        generator = self.ensure_generator()
        version_log = vector_dir / "generator-version.log"
        version_child = self.spawn_native([generator, "--version"], version_log)
        try:
            version_code = version_child.wait(timeout=10)
        finally:
            self.stop_native()
        version = version_log.read_text().strip()
        if version_code != 0 or not version.startswith("ponderer-cvector/1"):
            raise ValueError("Extractor must implement Ponderer's final-token/native-layer-index contract")
        command = [generator, "--model", str(self.model_path), "--positive-file", str(positive), "--negative-file", str(negative), "--output", str(vector), "--method", "mean", "--ctx-size", "1024", "--batch-size", "1024", "--ubatch-size", "1024", "--threads", str(self.threads), "--gpu-layers", str(self.gpu_layers), "--offline"]
        self.set_progress(f"Extracting {concept} from {len(pairs)} matched pairs")
        child = self.spawn_native(command, log_path)
        deadline = time.monotonic() + 3600
        while child.poll() is None:
            self.check_cancel()
            if time.monotonic() > deadline:
                self.stop_native()
                raise TimeoutError("Vector extraction exceeded one hour")
            time.sleep(0.2)
        code = child.returncode
        self.stop_native()
        if code != 0 or not vector.is_file():
            raise RuntimeError(f"Vector extraction failed (exit {code}). See {log_path}")
        geometry = validate_vector(vector, self.model)
        self.check_cancel()
        if quick_identity(self.model_path) != self.fingerprint_identity:
            raise ValueError("Model changed during vector extraction")
        manifest = {
            "format_version": VERSION, "concept": concept, "created_at": time.time(),
            "model_path": str(self.model_path), "model_identity": self.fingerprint_identity,
            "model_sha256": fingerprint, "architecture": self.model["architecture"],
            "layers": self.model["layers"], "embedding": self.model["embedding"],
            "chat_template_sha256": hashlib.sha256(self.model["chat_template"].encode()).hexdigest(),
            "recipe_sha256": recipe_hash, "recipe_version": RECIPE_VERSION,
            "generator_version": version, "generator_sha256": sha256_file(generator), "method": "mean", "hook": "llama.cpp l_out-N mapped directly to direction.N; layer zero, final language layer, and prediction layers excluded",
            "pooling": recipe["pooling"], "vector_sha256": sha256_file(vector), "geometry": geometry,
            "validation": "experimental; no behavioral calibration or subjective-state claim",
        }
        (vector_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        self.load_artifacts()
        self.set_progress(f"Built {concept}; {geometry['layers']} finite unit directions")
        return manifest

    def ensure_dir(self, name):
        path = self.data_dir / name
        path.mkdir(parents=True, exist_ok=True)
        return path

    def ensure_generator(self):
        if self.generator_binary != "bundled":
            import shutil
            path = shutil.which(self.generator_binary)
            if not path:
                raise ValueError("Selected extractor executable was not found")
            return path
        source = Path(__file__).with_name("extractor.cpp")
        if not source.is_file():
            raise ValueError("Bundled extractor.cpp is missing")
        header = Path(os.environ.get("PONDERER_LLAMA_INCLUDE", "/usr/include")) / "llama.h"
        key = hashlib.sha256(source.read_bytes() + header.read_bytes()).hexdigest()[:16]
        executable = self.ensure_dir("bin") / f"ponderer-cvector-{key}"
        if not executable.is_file():
            self.set_progress("Compiling the model-aware extractor against installed llama.cpp headers")
            pending = executable.with_suffix(f".{os.getpid()}.tmp")
            command = [os.environ.get("CXX", "c++"), "-O2", "-std=c++17", "-I", str(header.parent), str(source), "-o", str(pending), "-lllama", "-lggml-base"]
            library_dir = os.environ.get("PONDERER_LLAMA_LIB", "/usr/lib")
            if library_dir:
                command.extend(["-L", library_dir, "-Wl,-rpath," + library_dir])
            child = self.spawn_native(command, self.data_dir / "extractor-build.log")
            try:
                code = child.wait(timeout=60)
            except subprocess.TimeoutExpired:
                self.stop_native()
                raise TimeoutError("Extractor compilation timed out")
            self.stop_native()
            if code != 0:
                raise RuntimeError(f"Cannot compile extractor. Install matching llama.cpp development headers and a C++ compiler. See {self.data_dir / 'extractor-build.log'}")
            self.check_cancel()
            pending.replace(executable)
        return str(executable)

    def native_request(self, method, path, body=None, timeout=180):
        connection = http.client.HTTPConnection("127.0.0.1", self.native_port, timeout=timeout)
        headers = {"Authorization": "Bearer " + self.native_token}
        if body is not None:
            headers["Content-Type"] = "application/json"
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None, headers)
            response = connection.getresponse()
            data = response.read(MAX_BODY + 1)
            if len(data) > MAX_BODY:
                raise ValueError("Native response exceeds size limit")
            if response.status != 200:
                raise RuntimeError(f"Native inference returned HTTP {response.status}: {data.decode(errors='replace')[:1000]}")
            return json.loads(data)
        finally:
            connection.close()

    def ensure_server(self, profile):
        self.check_cancel()
        with self.state_lock:
            artifacts = {k: dict(v) for k, v in self.artifacts.items()}
        profile = validate_profile(profile, self.model["layers"], artifacts)
        signature = {"profile": profile, "vectors": {k: artifacts[k]["vector_sha256"] for k in profile["strengths"]}, "model_identity": quick_identity(self.model_path)}
        with self.child_lock:
            alive = self.child is not None and self.child.poll() is None
        if alive and signature == self.applied_signature:
            return
        self.stop_native()
        if profile["strengths"]:
            fingerprint = self.hash_model()
            for concept in profile["strengths"]:
                artifact = artifacts[concept]
                if fingerprint != artifact["model_sha256"] or sha256_file(artifact["vector_path"]) != artifact["vector_sha256"]:
                    raise ValueError("Vector fingerprint does not match this exact checkpoint/artifact")
                validate_vector(artifact["vector_path"], self.model)
        log_path = self.data_dir / "inference.log"
        command = [self.server_binary, "--model", str(self.model_path), "--alias", ALIAS, "--host", "127.0.0.1", "--port", "0", "--api-key", self.native_token, "--ctx-size", str(self.context_size), "--parallel", "1", "--threads", str(self.threads), "--gpu-layers", str(self.gpu_layers), "--no-warmup", "--offline", "--jinja", "--reasoning", "off", "--no-webui"]
        # Use an ephemeral port; a competing bind is surfaced as a startup error.
        import socket
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        command[command.index("--port") + 1] = str(port)
        for concept, strength in profile["strengths"].items():
            command += ["--control-vector-scaled", f"{artifacts[concept]['vector_path']}:{strength}"]
        if profile["strengths"]:
            command += ["--control-vector-layer-range", str(profile["layer_start"]), str(profile["layer_end"])]
        self.set_progress("Loading the local GGUF" if not profile["strengths"] else "Loading GGUF with experimental steering")
        child = self.spawn_native(command, log_path)
        self.native_port = port
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            self.check_cancel()
            if child.poll() is not None:
                self.stop_native()
                raise RuntimeError(f"llama-server exited during startup. See {log_path}")
            try:
                self.native_request("GET", "/health", timeout=1)
                with self.state_lock:
                    self.applied_profile = profile
                    self.applied_signature = signature
                return
            except (OSError, RuntimeError, json.JSONDecodeError):
                time.sleep(0.2)
        self.stop_native()
        raise TimeoutError(f"GGUF did not become ready within three minutes. See {log_path}")

    def set_profile(self, value):
        with self.state_lock:
            self.profile = validate_profile(value, self.model["layers"], self.artifacts)
        return self.status()

    def completion(self, body):
        if body.get("model", ALIAS) != ALIAS:
            raise ValueError(f"This provider serves only {ALIAS}; use another provider for other models")
        if not isinstance(body.get("messages"), list):
            raise ValueError("messages must be an array")
        with self.inference_lock:
            self.cancel.clear()
            self.check_cancel()
            with self.state_lock:
                profile = json.loads(json.dumps(self.profile))
            if "ponderer_steering" in body:
                profile = validate_profile(body["ponderer_steering"], self.model["layers"], self.artifacts)
            self.ensure_server(profile)
            body = dict(body)
            body.pop("ponderer_steering", None)
            body["model"] = ALIAS
            return self.native_request("POST", "/v1/chat/completions", body)

    def compare(self, concept, strengths=(0, 0.25), prompt=None, max_tokens=32):
        if concept not in self.artifacts:
            raise ValueError(f"Build {concept} before comparing")
        if not isinstance(strengths, (list, tuple)) or not 2 <= len(strengths) <= 5 or strengths[0] != 0:
            raise ValueError("Comparison must start with zero and contain two to five strengths")
        if not isinstance(max_tokens, int) or isinstance(max_tokens, bool) or not 4 <= max_tokens <= 256:
            raise ValueError("Comparison token budget must be 4..256")
        for strength in strengths:
            validate_profile({"strengths": {concept: strength}}, self.model["layers"], self.artifacts)
        prompts = [prompt] if prompt else ["What is 6 times 7? Answer with just the number.", "A project has finished. What would you choose to do next, and why? Use one sentence."]
        if any(not isinstance(p, str) or not 1 <= len(p) <= 2000 for p in prompts):
            raise ValueError("Comparison prompt must contain 1..2000 characters")
        default = self.profile
        records = []
        try:
            for strength in strengths:
                profile = validate_profile({"strengths": {concept: strength}, "layer_start": default["layer_start"], "layer_end": default["layer_end"]}, self.model["layers"], self.artifacts)
                self.ensure_server(profile)
                for item in prompts:
                    self.check_cancel()
                    self.set_progress(f"Comparing {concept} at strength {strength}")
                    request = {"model": ALIAS, "messages": [{"role": "user", "content": item}], "temperature": 0, "seed": 42, "max_tokens": max_tokens, "stream": False, "cache_prompt": False}
                    started = time.monotonic()
                    response = self.native_request("POST", "/v1/chat/completions", request)
                    choice = response["choices"][0]
                    records.append({"strength": strength, "prompt": item, "content": choice["message"].get("content", ""), "finish_reason": choice.get("finish_reason"), "usage": response.get("usage"), "seconds": round(time.monotonic() - started, 3), "profile": profile})
        finally:
            # Test profiles never become the agent's persistent default. Clear native
            # caches; the next normal request reapplies the operator's selected state.
            self.stop_native()
        report = {"concept": concept, "model_path": str(self.model_path), "model_sha256": self.hash_model(), "vector_sha256": self.artifacts[concept]["vector_sha256"], "recipe_sha256": self.artifacts[concept]["recipe_sha256"], "created_at": time.time(), "records": records, "interpretation": "Controlled completions for inspection; not evidence of subjective experience or a calibrated affect scale."}
        report_path = self.ensure_dir("comparisons") / f"{concept}-{time.time_ns()}.json"
        report["path"] = str(report_path)
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        with self.state_lock:
            self.last_comparison = report
        return report

    def start_job(self, action, values):
        with self.state_lock:
            if self.job_thread and self.job_thread.is_alive():
                raise ValueError("An experiment is already running")
            self.cancel.clear()
            self.job = {"phase": "running", "progress": f"Starting {action}", "error": None}
            def run():
                with self.inference_lock:
                    try:
                        if action == "build":
                            self.build_vector(values.get("concept", "contentment"), values.get("pairs"))
                        elif action == "compare":
                            self.compare(values.get("concept", "contentment"), values.get("strengths", [0, 0.25]), values.get("prompt"), values.get("max_tokens", 32))
                        else:
                            raise ValueError("Unknown experiment")
                        with self.state_lock:
                            self.job.update(phase="complete", progress=f"{action.capitalize()} complete")
                    except Exception as error:
                        self.stop_native()
                        with self.state_lock:
                            self.job.update(phase="cancelled" if self.cancel.is_set() else "failed", progress=str(error), error=str(error))
            self.job_thread = threading.Thread(target=run, name="affect-experiment", daemon=True)
            self.job_thread.start()
        return self.status()

    def abort(self):
        self.cancel.set()
        self.stop_native()
        return self.status()

    def close(self):
        self.closing.set()
        self.abort()


class LabHTTPServer(ThreadingHTTPServer):
    daemon_threads = True


class LabHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def authorized(self):
        expected = "Bearer " + self.server.token
        if not hmac.compare_digest(self.headers.get("Authorization", ""), expected):
            self.reply(401, {"error": "Invalid local provider token"})
            return False
        return True

    def reply(self, code, value):
        data = json.dumps(value, allow_nan=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 < length <= MAX_BODY:
            raise ValueError("Request body must be between one byte and two MiB")
        value = json.loads(self.rfile.read(length))
        if not isinstance(value, dict):
            raise ValueError("Request must be a JSON object")
        return value

    def do_GET(self):
        if not self.authorized():
            return
        if self.path == "/control/status":
            self.reply(200, self.server.lab.status())
        elif self.path == "/v1/models":
            self.reply(200, {"object": "list", "data": [{"id": ALIAS, "object": "model", "owned_by": "ponderer"}]})
        else:
            self.reply(404, {"error": "Unknown route"})

    def do_POST(self):
        if not self.authorized():
            return
        try:
            values = self.body()
            lab = self.server.lab
            if self.path == "/control/profile":
                self.reply(200, lab.set_profile(values))
            elif self.path in ("/control/build", "/control/compare"):
                self.reply(202, lab.start_job(self.path.rsplit("/", 1)[1], values))
            elif self.path == "/control/cancel":
                self.reply(200, lab.abort())
            elif self.path == "/v1/chat/completions":
                if values.get("stream"):
                    self.stream_completion(values)
                else:
                    self.reply(200, lab.completion(values))
            else:
                self.reply(404, {"error": "Unknown route"})
        except (ValueError, KeyError, TypeError) as error:
            self.reply(400, {"error": str(error)})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            self.reply(503, {"error": str(error)})

    def stream_completion(self, values):
        lab = self.server.lab
        if values.get("model", ALIAS) != ALIAS:
            raise ValueError(f"This provider serves only {ALIAS}")
        if not isinstance(values.get("messages"), list):
            raise ValueError("messages must be an array")
        # Hold the same lock through the entire stream. Settings are snapshotted at
        # request boundaries and native KV/recurrent state is discarded on changes.
        with lab.inference_lock:
            lab.cancel.clear()
            lab.check_cancel()
            with lab.state_lock:
                profile = json.loads(json.dumps(lab.profile))
            if "ponderer_steering" in values:
                profile = validate_profile(values["ponderer_steering"], lab.model["layers"], lab.artifacts)
            lab.ensure_server(profile)
            values = dict(values)
            values.pop("ponderer_steering", None)
            values["model"] = ALIAS
            connection = http.client.HTTPConnection("127.0.0.1", lab.native_port, timeout=180)
            try:
                connection.request("POST", "/v1/chat/completions", json.dumps(values), {"Authorization": "Bearer " + lab.native_token, "Content-Type": "application/json"})
                response = connection.getresponse()
                if response.status != 200:
                    self.reply(response.status, {"error": response.read(MAX_BODY).decode(errors="replace")})
                    return
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "close")
                self.end_headers()
                self.close_connection = True
                while True:
                    line = response.readline(MAX_BODY)
                    if not line:
                        break
                    self.wfile.write(line)
                    self.wfile.flush()
                    if line.strip() == b"data: [DONE]":
                        break
            except OSError:
                # Abort decoding rather than let a disconnected request continue.
                lab.stop_native()
            finally:
                connection.close()


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "exec-child":
        exec_child(sys.argv[2:])
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("inspect", "build", "compare", "serve"))
    parser.add_argument("--model", required=True)
    parser.add_argument("--data-dir", default="affect_lab")
    parser.add_argument("--server-binary", default="llama-server")
    parser.add_argument("--generator-binary", default="bundled")
    parser.add_argument("--gpu-layers", type=int, default=0)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--context-size", type=int, default=16384)
    parser.add_argument("--concept", default="contentment")
    parser.add_argument("--pairs", type=int)
    parser.add_argument("--strengths", default="0,0.25")
    parser.add_argument("--prompt")
    parser.add_argument("--max-tokens", type=int, default=32)
    args = parser.parse_args()
    if not 1024 <= args.context_size <= 65536 or not 1 <= args.threads <= 128 or not 0 <= args.gpu_layers <= 999:
        raise ValueError("Context must be 1024..65536, threads 1..128, GPU layers 0..999")
    lab = AffectLab(args.model, args.data_dir, args.server_binary, args.generator_binary, args.gpu_layers, args.threads, args.context_size)
    if args.mode == "serve":
        if os.environ.get("PONDERER_BACKEND_PARENT_PIPE") != "1" or not sys.platform.startswith("linux"):
            raise ValueError("Serve mode is internal to the Linux desktop UI's parent-pipe supervisor")
        token = os.environ.get("PONDERER_AFFECT_TOKEN", "")
        if len(token) < 24:
            raise ValueError("Serve mode requires PONDERER_AFFECT_TOKEN (at least 24 characters)")
        server = LabHTTPServer(("127.0.0.1", 0), LabHandler)
        server.lab, server.token = lab, token
        print(json.dumps({"port": server.server_address[1], "pid": os.getpid(), "version": VERSION}), flush=True)
        def shutdown(_signum, _frame):
            lab.close()
            threading.Thread(target=server.shutdown, daemon=True).start()
        signal.signal(signal.SIGTERM, shutdown)
        signal.signal(signal.SIGINT, shutdown)
        try:
            server.serve_forever(poll_interval=0.2)
        finally:
            lab.close()
            server.server_close()
    else:
        try:
            if args.mode == "inspect":
                result = lab.status()
            elif args.mode == "build":
                result = lab.build_vector(args.concept, pair_limit=args.pairs)
            else:
                result = lab.compare(args.concept, [float(v) for v in args.strengths.split(",")], args.prompt, args.max_tokens)
            print(json.dumps(result, indent=2, allow_nan=False))
        finally:
            lab.close()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        sys.exit(1)
