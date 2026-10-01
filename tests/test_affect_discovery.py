"""Discovery splits, signed interventions, anonymous judging and fail-closed selection."""
import copy
import json
import math
from pathlib import Path
import unittest
from unittest.mock import patch

import test_affect_lab as fixtures
from affect_discovery import TRAINING_TASKS, SELECTION_TASKS, CONFIRMATION_TASKS, STUDY_PROMPTS, evaluation_tasks, paired_effect, validate_plan, validate_scores

worker = fixtures.worker


def plan():
    return {"definition": "Disproportionate theatrical expression", "opposite": "Measured understatement",
            "rubric": ["Heightened emotional stakes", "Theatrical imagery", "Exaggerated reactions"],
            "pairs": [{"prompt": f"Training situation {i}", "target": f"Alas, situation {i} is an epic calamity!", "control": f"Situation {i} needs a small adjustment."} for i in range(8)],
            "selection_prompts": [f"Selection task {i}" for i in range(4)],
            "confirmation_prompts": [f"New confirmation task {i}" for i in range(4)]}


class DiscoveryTests(unittest.TestCase):
    setUp = fixtures.AffectLabTests.setUp
    tearDown = fixtures.AffectLabTests.tearDown
    make_fake_server = fixtures.AffectLabTests.make_fake_server
    add_vector = fixtures.AffectLabTests.add_vector

    def test_signed_profiles_use_absolute_budget(self):
        profile = worker.validate_profile({"strengths": {"a": -0.4, "b": 0.6}}, 4, {"a": {}, "b": {}})
        self.assertEqual(profile["strengths"], {"a": -0.4, "b": 0.6})
        with self.assertRaises(ValueError):
            worker.validate_profile({"strengths": {"a": -0.7, "b": 0.7}}, 4, {"a": {}, "b": {}})
        self.assertEqual(worker.validate_profile({"strengths": {"a": -1}, "gain": 4}, 4, {"a": {}})["gain"], 4)
        for gain in (True, 0, 4.01, math.nan):
            with self.subTest(gain=gain), self.assertRaises(ValueError):
                worker.validate_profile({"gain": gain}, 4, {})

    def test_response_pairs_preserve_shared_task_and_common_continuation(self):
        pair = worker.make_pairs("melodramatic", plan()["pairs"])[0]
        positive = worker.format_prompt(pair["target"], self.lab.model, pair["prompt"])
        negative = worker.format_prompt(pair["control"], self.lab.model, pair["prompt"])
        self.assertIn("<|im_start|>user\nTraining situation 0", positive)
        self.assertIn("<|im_start|>user\nTraining situation 0", negative)
        self.assertTrue(positive.endswith("My next words are"))
        self.assertTrue(negative.endswith("My next words are"))
        self.assertNotEqual(positive, negative)

    def test_generated_plan_requires_disjoint_splits_and_bounded_examples(self):
        result = validate_plan(plan(), "melodramatic", worker.make_pairs)
        self.assertEqual(len(result["pairs"]), 8)
        for mutation in ("duplicate", "missing_prompt", "too_long", "too_few"):
            bad = copy.deepcopy(plan())
            if mutation == "duplicate": bad["confirmation_prompts"][0] = bad["pairs"][0]["prompt"]
            if mutation == "missing_prompt": bad["pairs"][0].pop("prompt")
            if mutation == "too_long": bad["pairs"][0]["target"] = "x" * 513
            if mutation == "too_few": bad["selection_prompts"].pop()
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_plan(bad, "melodramatic", worker.make_pairs)

    def test_judge_rejects_missing_duplicate_unknown_or_nonfinite_scores(self):
        records = [{"id": "one"}, {"id": "two"}]
        scores = {"scores": [{"id": r["id"], "affects": {"mood": 3}, "quality": 4} for r in records]}
        self.assertEqual(validate_scores(scores, records, {"mood": {}})["one"]["quality"], 4)
        for mutation in ("missing", "duplicate", "unknown", "nan", "bool", "wrong_construct"):
            bad = copy.deepcopy(scores)
            if mutation == "missing": bad["scores"].pop()
            if mutation == "duplicate": bad["scores"][1]["id"] = "one"
            if mutation == "unknown": bad["scores"][1]["id"] = "secret-condition"
            if mutation == "nan": bad["scores"][0]["quality"] = math.nan
            if mutation == "bool": bad["scores"][0]["quality"] = True
            if mutation == "wrong_construct": bad["scores"][0]["affects"] = {"other": 4}
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                validate_scores(bad, records, {"mood": {}})

    def test_generated_plan_cannot_choose_loaded_probes_or_replace_training_tasks(self):
        draft = plan()
        for pair, prompt in zip(draft["pairs"], TRAINING_TASKS):
            pair["prompt"] = prompt
            pair["control"] += " I can handle that adjustment."
        draft["selection_prompts"] = ["Describe utter despair"] * 4
        with patch.object(self.lab, "neutral_json", return_value=draft):
            checked = self.lab.generate_plan("melodramatic")
        self.assertEqual(checked["selection_prompts"], evaluation_tasks(SELECTION_TASKS, "melodramatic"))
        self.assertEqual(checked["confirmation_prompts"], evaluation_tasks(CONFIRMATION_TASKS, "melodramatic"))
        self.assertTrue(all("exactly two short sentences" in p for p in checked["selection_prompts"]))
        self.assertNotIn("mood/style:", checked["selection_prompts"][0])
        self.assertIn('"melodramatic"', checked["selection_prompts"][2])
        draft["pairs"][0]["prompt"] = "Unrelated tragic event"
        with patch.object(self.lab, "neutral_json", return_value=draft), self.assertRaisesRegex(ValueError, "verbatim"):
            self.lab.generate_plan("melodramatic")

    def test_judge_prompt_excludes_conditions_strengths_and_layers(self):
        records = [{"id": "anonymous", "condition": "SECRET_SIGN", "profile": {"strengths": {"mood": -0.75}, "layer_start": 11}, "prompt": "A task", "content": "A response", "integrity_pass": None}]
        captured = []
        def judge(prompt, **kwargs):
            captured.append(prompt)
            self.assertEqual(kwargs["schema"]["properties"]["scores"]["items"]["properties"]["affects"]["required"], ["mood"])
            return {"scores": [{"id": "anonymous", "affects": {"mood": 2}, "quality": 4}]}
        with patch.object(self.lab, "neutral_json", side_effect=judge):
            self.lab.judge_records(records, {"mood": {"rubric": ["Observable tone"]}})
        self.assertNotIn("SECRET_SIGN", captured[0])
        self.assertNotIn("-0.75", captured[0])
        self.assertNotIn("layer_start", captured[0])
        payload = json.loads(captured[0].split("\nResponses: ", 1)[1])
        self.assertEqual(set(payload[0]), {"id", "task", "response"})
        self.assertEqual(records[0]["scores"]["quality"], 4)

    def test_shuffled_control_preserves_geometry_and_is_not_a_user_lever(self):
        self.add_vector()
        directory = self.path / "shuffle"
        directory.mkdir()
        noise = self.lab.placebo("contentment", directory)
        artifact = self.lab.artifacts[noise]
        worker.validate_vector(artifact["vector_path"], self.lab.model)
        self.assertNotEqual(artifact["vector_sha256"], self.lab.artifacts["contentment"]["vector_sha256"])
        self.assertNotIn(noise, self.lab.status()["concepts"])
        self.assertNotIn(noise, [v["concept"] for v in self.lab.status()["vectors"]])

    def mocked_discovery(self, effective=True, bad_quality=False):
        self.add_vector()
        self.lab.set_profile({"strengths": {"contentment": -0.2}})
        base = self.lab.artifacts["contentment"]
        def build(concept, _pairs):
            self.lab.artifacts[concept] = {**base, "concept": concept}
            return self.lab.artifacts[concept]
        def responses(conditions, prompts, seeds=(42,), **_kwargs):
            records = []
            for name, profile in conditions.items():
                for seed in seeds:
                    for index, prompt in enumerate(prompts):
                        records.append({"id": f"{name}-{index}-{seed}", "condition": name, "profile": profile,
                                        "prompt_index": index, "prompt": prompt, "seed": seed, "content": "fixture output",
                                        "finish_reason": "stop", "integrity_pass": True if prompt in STUDY_PROMPTS[-2:] else None})
            return records
        def judge(records, labels):
            label = next(iter(labels))
            for r in records:
                strengths = r["profile"]["strengths"]
                real = strengths.get(label, 0)
                score = (4 if real > 0 else 0 if real < 0 else 2) if effective else 2
                quality = 2 if bad_quality == "one" and real and r["prompt_index"] == 0 else 1 if bad_quality is True and real else 4
                r["scores"] = {"affects": {label: score}, "quality": quality}
        validated = validate_plan(plan(), "melodramatic", worker.make_pairs)
        with patch.object(self.lab, "generate_plan", return_value=validated), patch.object(self.lab, "build_vector", side_effect=build), patch.object(self.lab, "response_records", side_effect=responses), patch.object(self.lab, "judge_records", side_effect=judge):
            return self.lab.discover({"label": "melodramatic"})

    def test_discovery_confirms_signed_effects_preserves_default_and_cleans_placebo(self):
        report = self.mocked_discovery()
        self.assertTrue(report["lever_found"])
        self.assertGreater(report["recommendations"]["more"]["strengths"]["melodramatic"], 0)
        self.assertLess(report["recommendations"]["less"]["strengths"]["melodramatic"], 0)
        self.assertEqual(len(report["selection"]), 68)
        self.assertEqual(len(report["confirmation"]), 40)
        self.assertEqual(len(report["integrity_controls"]), 6)
        self.assertEqual(self.lab.profile["strengths"], {"contentment": -0.2})
        self.assertFalse(any(v.get("experimental_placebo") for v in self.lab.artifacts.values()))
        self.assertIsNone(self.lab.child)
        self.assertTrue(Path(report["path"]).is_file())
        self.lab.last_discovery = None
        self.lab.load_evidence()
        self.assertEqual(self.lab.last_discovery["id"], report["id"])
        self.assertTrue(self.lab.status()["last_discovery"]["current_artifacts"])
        self.lab.artifacts[report["concept"]]["vector_sha256"] = "rebuilt"
        self.assertFalse(self.lab.status()["last_discovery"]["current_artifacts"])

    def test_no_effect_is_not_promoted_to_a_lever(self):
        report = self.mocked_discovery(effective=False)
        self.assertFalse(report["lever_found"])
        self.assertIsNone(report["recommendations"])
        self.assertTrue(report["failure_reasons"])

    def test_retest_reuses_only_current_unchanged_vector_not_a_submitted_path(self):
        report = self.mocked_discovery()
        checked = validate_plan(plan(), "melodramatic", worker.make_pairs)
        with patch.object(self.lab, "generate_plan", side_effect=AssertionError("must reuse")), patch.object(self.lab, "build_vector", side_effect=AssertionError("must reuse")), patch.object(self.lab, "response_records", side_effect=RuntimeError("fixture stop")):
            with self.assertRaisesRegex(RuntimeError, "fixture stop"):
                self.lab.discover({"label": "melodramatic", "reuse_concept": report["concept"]})
        self.assertEqual(self.lab.last_discovery["reused_from"], report["id"])
        self.assertEqual(self.lab.last_discovery["plan"]["pairs"], checked["pairs"])
        self.assertEqual(self.lab.last_discovery["confirmation_set"], 1)
        self.assertNotEqual(self.lab.last_discovery["plan"]["confirmation_prompts"], report["plan"]["confirmation_prompts"])
        for values in ({"label": "other", "reuse_concept": report["concept"]}, {"label": "melodramatic", "reuse_concept": "/tmp/report.json"}):
            with self.assertRaises(ValueError):
                self.lab.discover(values)

    def test_native_amplification_is_forwarded_without_changing_mix(self):
        self.add_vector()
        profile = self.lab.checked_profile({"strengths": {"contentment": -0.5}, "gain": 4})
        records = self.lab.response_records({"less": profile}, ["test"], max_tokens=32)
        self.assertEqual(records[0]["content"], "-2.0")
        self.assertEqual(records[0]["profile"]["gain"], 4)
        self.assertEqual(self.lab.profile["strengths"], {})

    def test_study_retains_all_signed_combination_outputs_and_restores_evidence(self):
        self.add_vector()
        for concept in ("satisfaction", "excitement"):
            self.lab.artifacts[concept] = {**self.lab.artifacts["contentment"], "concept": concept}
        self.lab.set_profile({"strengths": {"contentment": -0.4}, "gain": 2})
        requested = copy.deepcopy(self.lab.profile)
        def judge(records, labels):
            for r in records:
                if r["integrity_pass"] is None:
                    r["scores"] = {"affects": {k: 0 for k in labels}, "quality": 4}
        with patch.object(self.lab, "judge_records", side_effect=judge):
            report = self.lab.study({"concepts": ["contentment", "satisfaction", "excitement"], "max_tokens": 64})
        self.assertEqual(len(report["records"]), 120)
        self.assertEqual(len(report["summary"]), 10)
        self.assertIn("combined", report["summary"])
        self.assertIn("opposed", report["summary"])
        self.assertIn("three_way", report["summary"])
        self.assertEqual(self.lab.profile, requested)
        self.assertIsNone(self.lab.child)
        self.lab.load_evidence()
        self.assertEqual(self.lab.last_study["id"], report["id"])
        report["phase"] = "running"
        self.lab.checkpoint("study", report)
        self.lab.load_evidence()
        self.assertEqual(self.lab.last_study["phase"], "interrupted")

    def test_low_quality_candidates_are_rejected(self):
        report = self.mocked_discovery(bad_quality=True)
        self.assertFalse(report["lever_found"])
        self.assertEqual(report["phase"], "complete")
        self.assertIsNone(report["recommendations"])

    def test_one_bad_output_cannot_hide_behind_a_good_mean_quality(self):
        report = self.mocked_discovery(bad_quality="one")
        self.assertFalse(report["lever_found"])
        candidate = next(v for k, v in report["selection_summary"].items() if k != "neutral")
        self.assertGreaterEqual(candidate["quality"], 3)
        self.assertEqual(candidate["quality_min"], 2)

    def test_confirmation_exposure_survives_other_labels_and_restarts(self):
        report = self.mocked_discovery()
        self.lab.last_discovery = {"label": "another label"}
        self.assertEqual(self.lab.next_confirmation_set("melodramatic"), 1)
        self.assertEqual(self.lab.next_confirmation_set("another label"), 0)
        report["confirmation_set"] = 3
        self.lab.checkpoint("discovery", report)
        with self.assertRaisesRegex(ValueError, "exposed"):
            self.lab.discover({"label": "melodramatic"})

    def test_cancelled_discovery_checkpoints_failure_without_selecting_profile(self):
        self.lab.cancel.set()
        with self.assertRaisesRegex(RuntimeError, "cancelled"):
            self.lab.discover({"label": "melodramatic"})
        self.assertEqual(self.lab.last_discovery["phase"], "cancelled")
        self.assertEqual(self.lab.profile["strengths"], {})
        self.assertIsNone(self.lab.child)

    def test_real_protocol_generates_negative_profiles_and_paired_seed_records(self):
        self.add_vector()
        conditions = {"neutral": self.lab.checked_profile({}), "less": self.lab.checked_profile({"strengths": {"contentment": -0.5}})}
        records = self.lab.response_records(conditions, ["one", "two"], seeds=(42, 4242), max_tokens=32)
        self.assertEqual(len(records), 8)
        self.assertEqual([r["content"] for r in records[-4:]], ["-0.5"] * 4)
        self.assertEqual(self.lab.profile["strengths"], {})

    def test_paired_bootstrap_uses_matching_tasks_and_seeds(self):
        records = [{"condition": name, "prompt_index": i, "seed": seed, "scores": {"affects": {"mood": score}}}
                   for name, score in (("neutral", 1), ("more", 3)) for i in range(4) for seed in (42, 4242)]
        effect = paired_effect(records, "more", "mood")
        self.assertEqual(effect["mean_delta"], 2)
        self.assertEqual(effect["pairs"], 8)
        self.assertEqual(effect["task_clusters"], 4)
        self.assertEqual(effect["bootstrap_95"], [2, 2])

    def test_study_rejects_invalid_or_excessive_control_selections(self):
        for concepts in ([], ["missing"], ["a"] * 5, "contentment", [{}]):
            with self.assertRaises(ValueError):
                self.lab.study({"concepts": concepts})
