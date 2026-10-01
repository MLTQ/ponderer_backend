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


def make_gguf(path, vector=False, values=(1.0, 0.0, 0.0), architecture="qwen35"):
    metadata = {"general.architecture": "controlvector" if vector else architecture}
    if vector:
        metadata["controlvector.model_hint"] = "qwen35"
    else:
        metadata.update({f"{architecture}.block_count": 5, f"{architecture}.nextn_predict_layers": 1, f"{architecture}.embedding_length": 3, "tokenizer.chat_template": "<|im_start|>user", "general.name": "Test model"})
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
    except (FileNotFoundError, ProcessLookupError):
        return True


def fake_server():
    if "--list-devices" in sys.argv:
        print("Available devices:\n  CUDA0: Fixture GPU (24000 MiB, 20000 MiB free)\n  CUDA1: Fixture small GPU (8000 MiB, 2000 MiB free)")
        return
    def option(name):
        return sys.argv[sys.argv.index(name) + 1]
    port = int(option("--port"))
    strength = option("--control-vector-scaled").rsplit(":", 1)[1] if "--control-vector-scaled" in sys.argv else "0"
    # Record only memory settings, never process arguments containing API tokens.
    settings = {key: option(key) for key in ("--ctx-size", "--cache-type-k", "--cache-type-v", "--flash-attn", "--timeout")}
    settings["unified_kv_cache"] = "--kv-unified" in sys.argv
    settings["context_shift"] = "--no-context-shift" not in sys.argv
    placement = {key: option(key) for key in ("--gpu-layers", "--device", "--split-mode", "--main-gpu", "--fit")}
    template = option("--chat-template-file") if "--chat-template-file" in sys.argv else None
    controls = option("--control-vector-scaled").split(",") if "--control-vector-scaled" in sys.argv else []
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def do_GET(self):
            data = json.dumps({"status": "ok", "test_settings": settings, "test_placement": placement, "test_chat_template": template, "test_control_vectors": controls, "test_control_flag_count": sys.argv.count("--control-vector-scaled")}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            time.sleep(min(2, max(0, float(os.environ.get("PONDERER_AFFECT_TEST_DELAY", "0")))))
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

    def add_vector(self, concept="contentment"):
        directory = self.path / f"lab/vectors/{concept}-test"
        directory.mkdir(parents=True)
        vector = directory / "vector.gguf"
        make_gguf(vector, vector=True)
        manifest = {"concept": concept, "created_at": time.time(), "model_path": str(self.model), "model_identity": worker.quick_identity(self.model), "model_sha256": worker.sha256_file(self.model), "recipe_sha256": "recipe", "vector_sha256": worker.sha256_file(vector)}
        (directory / "manifest.json").write_text(json.dumps(manifest))
        self.lab.load_artifacts()
        return vector

    def completion(self, **extra):
        return self.lab.completion({"model": worker.ALIAS, "messages": [{"role": "user", "content": "test"}], **extra})

    def test_metadata_excludes_prediction_layer(self):
        self.assertEqual(self.lab.model["layers"], 4)
        self.assertEqual(self.lab.model["prediction_layers"], 1)

    def test_qwen_tool_template_is_forwarded_and_fingerprinted(self):
        self.completion()
        health = self.lab.native_request("GET", "/health")
        self.assertEqual(health["test_chat_template"], str(self.lab.chat_template_path))
        template = self.lab.chat_template_path.read_text()
        self.assertIn('message.role == "tool"', template)
        self.assertIn('message.tool_calls', template)
        self.assertIn('enable_thinking', template)
        self.assertEqual(self.lab.inference_settings()["chat_template_sha256"], worker.sha256_file(self.lab.chat_template_path))
        self.lab.close()
        model = self.path / "other.gguf"
        make_gguf(model, architecture="llama")
        self.lab = worker.AffectLab(model, self.path / "other-lab", server_binary=str(self.make_fake_server()))
        self.completion()
        self.assertIsNone(self.lab.native_request("GET", "/health")["test_chat_template"])
        self.assertEqual(self.lab.inference_settings()["chat_template"], "embedded")

    def test_all_mix_controls_use_one_native_flag(self):
        vectors = [self.add_vector(concept) for concept in ("contentment", "excitement", "fear")]
        self.lab.set_profile({"strengths": {"contentment": 0.2, "excitement": -0.3, "fear": 0.1}, "gain": 2})
        self.completion()
        health = self.lab.native_request("GET", "/health")
        self.assertEqual(health["test_control_flag_count"], 1)
        expected = {f"{path}:{strength}" for path, strength in zip(vectors, (0.4, -0.6, 0.2))}
        self.assertEqual(set(health["test_control_vectors"]), expected)

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
        for changes in ({"context_size": worker.MAX_CONTEXT_SIZE + 1}, {"context_size": 0}, {"context_size": True}, {"unified_kv_cache": "true"}, {"cache_type_k": "q4_k_m"}, {"flash_attention": "off"}, {"flash_attention": "yes"}, {"gpu_layers": -2}, {"gpu_layers": -1}, {"gpu_layers": 100}, {"gpu_device": "CUDA0,CUDA1"}, {"gpu_device": "none"}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                worker.validate_inference_settings(**(defaults | changes))

    def test_all_gpu_placement_reaches_native_without_fallback_and_survives_reload(self):
        self.lab.close()
        self.lab = worker.AffectLab(self.model, self.path / "lab", server_binary=str(self.make_fake_server()), gpu_layers=-1, gpu_device="CUDA0", context_size=200_000, cache_type_k="q4_1", cache_type_v="q4_1", flash_attention="on")
        self.add_vector()
        for strength in (0, 0.25):
            self.lab.set_profile({"strengths": {"contentment": strength}})
            self.completion()
            health = self.lab.native_request("GET", "/health")
            self.assertEqual(health["test_placement"], {"--gpu-layers": "all", "--device": "CUDA0", "--split-mode": "none", "--main-gpu": "0", "--fit": "off"})
            self.assertEqual(health["test_settings"]["--ctx-size"], "200000")
            self.assertEqual(health["test_settings"]["--cache-type-k"], "q4_1")
        self.assertEqual(self.lab.inference_settings()["gpu_offload"], "all")
        self.assertEqual(self.lab.inference_settings()["gpu_device"], "CUDA0")
        previous = self.lab.child.pid
        self.lab.gpu_device = "CUDA1"
        self.completion()
        self.assertNotEqual(previous, self.lab.child.pid)
        self.assertEqual(self.lab.native_request("GET", "/health")["test_placement"]["--device"], "CUDA1")

    def test_cpu_mode_and_explicit_partial_offload_remain_available(self):
        self.completion()
        self.assertEqual(self.lab.native_request("GET", "/health")["test_placement"]["--device"], "none")
        self.lab.close()
        self.lab = worker.AffectLab(self.model, self.path / "lab", server_binary=str(self.make_fake_server()), gpu_layers=17, gpu_device="CUDA1")
        self.completion()
        placement = self.lab.native_request("GET", "/health")["test_placement"]
        self.assertEqual(placement["--gpu-layers"], "17")
        self.assertEqual(placement["--device"], "CUDA1")
        self.assertEqual(placement["--fit"], "off")

    def test_oom_reports_selected_gpu_and_preserves_requested_settings(self):
        self.lab.close()
        failing=self.path / "oom-server"
        failing.write_text("#!/bin/sh\nprintf 'cudaMalloc failed: out of memory\\n' >&2\nexit 1\n")
        failing.chmod(0o700)
        self.lab = worker.AffectLab(self.model, self.path / "lab", server_binary=str(failing), gpu_layers=-1, gpu_device="CUDA0", context_size=200_000)
        with self.assertRaisesRegex(RuntimeError, "GPU memory exhausted on CUDA0 with all GPU layers and 200000 context tokens"):
            self.completion()
        self.assertEqual(self.lab.gpu_layers, -1)
        self.assertEqual(self.lab.context_size, 200_000)
        self.assertIsNone(self.lab.child)

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
        for strengths in ({"contentment": math.nan}, {"contentment": math.inf}, {"contentment": True}, {"contentment": -1.1}, {"contentment": 1.1}, {"contentment": 0.6, "excitement": 0.6}, {"contentment": -0.6, "excitement": 0.6}):
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

    def test_example_library_exposes_reviewable_starters_and_built_recipe(self):
        self.add_vector()
        directory = Path(self.lab.artifacts["contentment"]["vector_path"]).parent
        recipe = {"pairs": worker.make_pairs("contentment")}
        (directory / "recipe.json").write_text(json.dumps(recipe))
        manifest = json.loads((directory / "manifest.json").read_text())
        manifest["recipe_sha256"] = worker.hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest()
        (directory / "manifest.json").write_text(json.dumps(manifest))
        self.lab.load_artifacts()
        library = {v["concept"]: v for v in self.lab.status()["example_library"]}
        self.assertEqual(library["contentment"]["source"], "built recipe")
        self.assertEqual(library["contentment"]["pairs"], recipe["pairs"])
        self.assertFalse(library["fear"]["built"])
        self.assertEqual(len(library["curiosity"]["pairs"]), 8)
        recipe["pairs"][0]["target"] = "tampered"
        (directory / "recipe.json").write_text(json.dumps(recipe))
        self.lab.load_artifacts()
        self.assertNotIn("contentment", self.lab.recipes)

    def test_mix_comparison_preserves_default_and_fingerprints_all_controls(self):
        self.add_vector()
        self.lab.artifacts["excitement"] = {**self.lab.artifacts["contentment"], "concept": "excitement"}
        self.lab.set_profile({"strengths": {"contentment": 0.1}})
        profile = {"strengths": {"contentment": 0.2, "excitement": 0.3}, "layer_start": 1, "layer_end": 2}
        report = self.lab.compare_mix(profile, ["held-out task", worker.TEST_PROMPTS[3]], 4)
        self.assertEqual([r["strength"] for r in report["records"]], [0, 0, 0.5, 0.5, 1, 1])
        self.assertEqual(report["records"][-1]["profile"]["strengths"], profile["strengths"])
        self.assertEqual(set(report["vectors"]), {"contentment", "excitement"})
        self.assertEqual(report["generation"]["seed"], 42)
        self.assertIsNone(report["records"][0]["integrity_pass"])
        self.assertFalse(report["records"][1]["integrity_pass"])
        self.assertEqual(self.lab.profile["strengths"], {"contentment": 0.1})
        self.assertIsNone(self.lab.child)

    def test_mix_validation_and_assessment_are_bounded_and_report_specific(self):
        self.add_vector()
        for profile, prompts in (({}, ["test"]), ({"strengths": {"contentment": 0.2}}, []), ({"strengths": {"contentment": 0.2}}, ["test"] * 7)):
            with self.assertRaises(ValueError):
                self.lab.compare_mix(profile, prompts, 4)
        report = self.lab.compare_mix({"strengths": {"contentment": 0.2}}, ["test"], 4)
        values = {"id": report["id"], "affect": "mixed", "quality": "yes", "notes": "Changed words, not enough evidence"}
        self.lab.review_comparison(values)
        saved = json.loads(Path(report["path"]).read_text())
        self.assertEqual(saved["review"]["affect"], "mixed")
        self.assertIn("operator judgment", saved["review"]["source"])
        for changes in ({"id": "older-report"}, {"affect": "validated"}, {"notes": "x" * 4001}):
            with self.assertRaises(ValueError):
                self.lab.review_comparison(values | changes)

    def test_explicit_load_job_allocates_and_cancelled_queued_load_stays_cancelled(self):
        self.lab.start_job("load", {})
        self.lab.job_thread.join(10)
        self.assertEqual(self.lab.job["phase"], "complete")
        time.sleep(0.2)
        self.assertIsNotNone(self.lab.status()["native_pid"])
        with self.lab.inference_lock:
            self.lab.start_job("load", {})
            self.lab.abort()
            # An intervening normal request may clear the global cancel flag.
            self.lab.cancel.clear()
        self.lab.job_thread.join(10)
        self.assertEqual(self.lab.job["phase"], "cancelled")
        self.assertIsNone(self.lab.child)

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
