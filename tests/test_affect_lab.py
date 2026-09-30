"""Protocol, fingerprint, request isolation and native-child lifetime tests."""
import importlib.util
import json
import math
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WORKER = Path(__file__).resolve().parents[1] / "resources/affect_lab/worker.py"
spec = importlib.util.spec_from_file_location("affect_worker", WORKER)
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


def gguf_string(text):
    data = text.encode()
    return struct.pack("<Q", len(data)) + data


def make_gguf(path, vector=False, values=(1.0, 0.0, 0.0)):
    metadata = {"general.architecture": "controlvector" if vector else "qwen35"}
    if vector:
        metadata["controlvector.model_hint"] = "qwen35"
    else:
        metadata.update({"qwen35.block_count": 5, "qwen35.nextn_predict_layers": 1, "qwen35.embedding_length": 3, "tokenizer.chat_template": "<|im_start|>user", "general.name": "Test model"})
    data = b"GGUF" + struct.pack("<IQQ", 3, 2 if vector else 0, len(metadata))
    for key, value in metadata.items():
        kind = 8 if isinstance(value, str) else 4
        data += gguf_string(key) + struct.pack("<I", kind)
        data += gguf_string(value) if kind == 8 else struct.pack("<I", value)
    if vector:
        for layer in (1, 2):
            data += gguf_string(f"direction.{layer}") + struct.pack("<IQIQ", 1, 3, 0, (layer - 1) * 32)
        data += b"\0" * ((-len(data)) % 32)
        packed = struct.pack("<fff", *values)
        data += packed + b"\0" * 20 + packed
    Path(path).write_bytes(data)


def inactive(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_text().split(")", 1)[1].split()[0]
        return state == "Z"
    except FileNotFoundError:
        return True


def fake_server():
    def option(name):
        return sys.argv[sys.argv.index(name) + 1]
    port = int(option("--port"))
    strength = option("--control-vector-scaled").rsplit(":", 1)[1] if "--control-vector-scaled" in sys.argv else "0"
    # Record only memory settings, never process arguments containing API tokens.
    settings = {key: option(key) for key in ("--ctx-size", "--cache-type-k", "--cache-type-v", "--flash-attn", "--timeout")}
    settings["unified_kv_cache"] = "--kv-unified" in sys.argv
    settings["context_shift"] = "--no-context-shift" not in sys.argv
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def do_GET(self):
            data = json.dumps({"status": "ok", "test_settings": settings}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            data = {"choices": [{"message": {"role": "assistant", "content": strength}, "finish_reason": "stop"}], "usage": {"completion_tokens": 1}}
            if body.get("stream"):
                data = 'data: ' + json.dumps({"choices": [{"delta": {"content": strength}}]}) + '\n\ndata: [DONE]\n\n'
                data = data.encode()
            else:
                data = json.dumps(data).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


class AffectLabTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ponderer-affect-test-")
        self.path = Path(self.temp.name)
        self.model = self.path / "model.gguf"
        make_gguf(self.model)
        self.lab = worker.AffectLab(self.model, self.path / "lab", server_binary=str(self.make_fake_server()))

    def tearDown(self):
        self.lab.close()
        self.temp.cleanup()

    def make_fake_server(self):
        path = self.path / "fake-server"
        path.write_text(f"#!/bin/sh\nexec {sys.executable} {Path(__file__).resolve()} --fake-server \"$@\"\n")
        path.chmod(0o700)
        return path

    def add_vector(self):
        directory = self.path / "lab/vectors/contentment-test"
        directory.mkdir(parents=True)
        vector = directory / "vector.gguf"
        make_gguf(vector, vector=True)
        manifest = {"concept": "contentment", "created_at": time.time(), "model_path": str(self.model), "model_identity": worker.quick_identity(self.model), "model_sha256": worker.sha256_file(self.model), "recipe_sha256": "recipe", "vector_sha256": worker.sha256_file(vector)}
        (directory / "manifest.json").write_text(json.dumps(manifest))
        self.lab.load_artifacts()
        return vector

    def completion(self, **extra):
        return self.lab.completion({"model": worker.ALIAS, "messages": [{"role": "user", "content": "test"}], **extra})

    def test_metadata_excludes_prediction_layer(self):
        self.assertEqual(self.lab.model["layers"], 4)
        self.assertEqual(self.lab.model["prediction_layers"], 1)

    def test_long_context_settings_reach_the_native_engine_and_report(self):
        self.lab.close()
        self.lab = worker.AffectLab(self.model, self.path / "lab", server_binary=str(self.make_fake_server()), context_size=200_000, unified_kv_cache=True, cache_type_k="q4_1", cache_type_v="q4_1", flash_attention="on")
        self.add_vector()
        self.completion()
        settings = self.lab.native_request("GET", "/health")["test_settings"]
        self.assertEqual(settings, {"--ctx-size": "200000", "--cache-type-k": "q4_1", "--cache-type-v": "q4_1", "--flash-attn": "on", "--timeout": "3600", "unified_kv_cache": True, "context_shift": False})
        report = self.lab.compare("contentment", [0, 0.1], "test", 4)
        self.assertEqual(report["inference_settings"], self.lab.status()["inference_settings"])
        self.assertEqual(report["inference_settings"]["context_size"], 200_000)
        self.lab.unified_kv_cache = False
        self.completion()
        first = self.lab.child.pid
        self.assertFalse(self.lab.native_request("GET", "/health")["test_settings"]["unified_kv_cache"])
        self.lab.unified_kv_cache = True
        self.completion()
        self.assertNotEqual(first, self.lab.child.pid)

    def test_invalid_memory_settings_are_rejected_before_launch(self):
        defaults = dict(context_size=200_000, gpu_layers=0, threads=4, unified_kv_cache=True, cache_type_k="q4_1", cache_type_v="q4_1", flash_attention="on")
        worker.validate_inference_settings(**defaults)
        for changes in ({"context_size": worker.MAX_CONTEXT_SIZE + 1}, {"context_size": 0}, {"context_size": True}, {"unified_kv_cache": "true"}, {"cache_type_k": "q4_k_m"}, {"flash_attention": "off"}, {"flash_attention": "yes"}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                worker.validate_inference_settings(**(defaults | changes))

    def test_long_prompt_body_is_accepted_beyond_the_old_two_mib_limit(self):
        server = worker.LabHTTPServer(("127.0.0.1", 0), worker.LabHandler)
        server.lab, server.token = self.lab, "fixture-token" * 3
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            body = json.dumps({"model": worker.ALIAS, "messages": [{"role": "user", "content": "a" * (worker.MAX_BODY + 1)}]}).encode()
            request = Request(f"http://127.0.0.1:{server.server_address[1]}/v1/chat/completions", body, {"Authorization": "Bearer " + server.token, "Content-Type": "application/json"})
            with urlopen(request, timeout=10) as response:
                self.assertEqual(json.load(response)["choices"][0]["message"]["content"], "0")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_extraction_describes_assistant_state_at_common_continuation(self):
        pair = worker.make_pairs("contentment")[0]
        target, control = [worker.format_prompt(pair[key], self.lab.model) for key in ("target", "control")]
        self.assertNotIn("<|im_start|>user", target)
        self.assertIn("<|im_start|>assistant\n" + pair["target"], target)
        self.assertTrue(target.endswith("My next step is"))
        self.assertTrue(control.endswith("My next step is"))

    def test_directory_excludes_projector_and_rejects_ambiguity(self):
        make_gguf(self.path / "mmproj-model.gguf")
        self.assertEqual(worker.resolve_model(self.path), self.model)
        make_gguf(self.path / "second.gguf")
        with self.assertRaisesRegex(ValueError, "multiple"):
            worker.resolve_model(self.path)

    def test_invalid_profiles_are_rejected(self):
        for strengths in ({"contentment": math.nan}, {"contentment": math.inf}, {"contentment": True}, {"contentment": -0.1}, {"contentment": 1.1}, {"contentment": 0.6, "excitement": 0.6}):
            with self.subTest(strengths=strengths), self.assertRaises(ValueError):
                worker.validate_profile({"strengths": strengths}, 4, {"contentment": {}, "excitement": {}})
        with self.assertRaises(ValueError):
            worker.validate_profile({"strengths": {"missing": 0.1}}, 4, {})
        with self.assertRaises(ValueError):
            worker.validate_profile({"layer_end": 3}, 4, {})

    def test_vector_geometry_rejects_nonfinite_and_unnormalized_values(self):
        for values in ((math.nan, 0, 0), (0, 0, 0), (2, 0, 0)):
            vector = self.path / "bad.gguf"
            make_gguf(vector, vector=True, values=values)
            with self.assertRaises(ValueError):
                worker.validate_vector(vector, self.lab.model)

    def test_vectors_require_exact_checkpoint_and_content_hashes(self):
        vector = self.add_vector()
        self.lab.artifacts["contentment"]["model_sha256"] = "wrong"
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            self.lab.ensure_server(worker.validate_profile({"strengths": {"contentment": 0.25}}, 4, self.lab.artifacts))
        self.lab.artifacts["contentment"]["model_sha256"] = worker.sha256_file(self.model)
        with vector.open("ab") as handle:
            handle.write(b"changed")
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            self.lab.ensure_server(worker.validate_profile({"strengths": {"contentment": 0.25}}, 4, self.lab.artifacts))

    def test_request_profiles_do_not_leak_or_reuse_incompatible_state(self):
        self.add_vector()
        self.assertEqual(self.completion()["choices"][0]["message"]["content"], "0")
        first = self.lab.child.pid
        self.assertEqual(self.completion()["choices"][0]["message"]["content"], "0")
        self.assertEqual(self.lab.child.pid, first)
        result = self.completion(ponderer_steering={"strengths": {"contentment": 0.5}})
        self.assertEqual(result["choices"][0]["message"]["content"], "0.5")
        self.assertNotEqual(self.lab.child.pid, first)
        self.assertEqual(self.lab.profile["strengths"], {})
        self.assertEqual(self.completion()["choices"][0]["message"]["content"], "0")

    def test_cancel_allows_subsequent_requests_and_stop_reaps_native(self):
        self.completion()
        pid = self.lab.child.pid
        self.lab.abort()
        self.assertTrue(inactive(pid))
        self.assertEqual(self.completion()["choices"][0]["message"]["content"], "0")

    def test_comparison_restores_manual_profile_and_records_evidence(self):
        self.add_vector()
        self.lab.set_profile({"strengths": {"contentment": 0.1}})
        report = self.lab.compare("contentment", [0, 0.5], "test", 4)
        self.assertEqual([v["content"] for v in report["records"]], ["0", "0.5"])
        self.assertEqual(self.lab.profile["strengths"], {"contentment": 0.1})
        self.assertIsNone(self.lab.child)
        self.assertTrue(Path(report["path"]).is_file())
        self.assertEqual(self.completion()["choices"][0]["message"]["content"], "0.1")

    def test_http_authentication_and_streaming(self):
        server = worker.LabHTTPServer(("127.0.0.1", 0), worker.LabHandler)
        server.lab, server.token = self.lab, "test-token" * 4
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base = f"http://127.0.0.1:{server.server_address[1]}"
        try:
            with self.assertRaises(HTTPError) as failure:
                urlopen(base + "/control/status", timeout=2)
            self.assertEqual(failure.exception.code, 401)
            failure.exception.close()
            body = json.dumps({"model": worker.ALIAS, "messages": [], "stream": True}).encode()
            request = Request(base + "/v1/chat/completions", body, {"Authorization": "Bearer " + server.token, "Content-Type": "application/json"})
            with urlopen(request, timeout=5) as response:
                stream = response.read().decode()
            self.assertIn('"content": "0"', stream)
            self.assertIn("data: [DONE]", stream)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux parent-death safeguard")
    def test_native_child_dies_if_owning_worker_is_killed(self):
        marker = self.path / "native-ready"
        code = f"import importlib.util,subprocess,sys,time; s=importlib.util.spec_from_file_location('w',{str(WORKER)!r}); w=importlib.util.module_from_spec(s); s.loader.exec_module(w); p=subprocess.Popen(w.native_child_command([sys.executable,'-c',\"from pathlib import Path; import os,time; Path({str(marker)!r}).write_text(str(os.getpid())); time.sleep(60)\"]),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); print(p.pid,flush=True); time.sleep(60)"
        parent = subprocess.Popen([sys.executable, "-u", "-c", code], stdout=subprocess.PIPE, text=True)
        pid = int(parent.stdout.readline())
        try:
            deadline = time.monotonic() + 3
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "Native child must establish parent-death signaling before testing")
            native_pid = int(marker.read_text())
            parent.kill()
            parent.wait(timeout=3)
            deadline = time.monotonic() + 3
            while not inactive(pid) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(inactive(pid), "Native child survived owning-worker death")
            deadline = time.monotonic() + 3
            while not inactive(native_pid) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(inactive(native_pid), "Native process survived supervisor death")
        finally:
            if parent.poll() is None:
                parent.kill()
                parent.wait(timeout=3)
            parent.stdout.close()
            if not inactive(pid):
                os.kill(pid, signal.SIGKILL)


if __name__ == "__main__":
    if "--fake-server" in sys.argv:
        fake_server()
    elif len(sys.argv) == 3 and sys.argv[1] == "--make-model":
        make_gguf(sys.argv[2])
    else:
        unittest.main()
