from __future__ import annotations

import contextlib
import io
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from yukinal_knowledge import cli
from yukinal_knowledge.store import APPLICATION_ID, KnowledgeStore


def write_document(path: Path, title: str, body: str, *, environment: str = "demo") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "+++\n"
        f'title = "{title}"\n'
        'source = "https://example.invalid/reference"\n'
        'updated_at = "2026-11-25"\n'
        f'environment = "{environment}"\n'
        'server_tags = ["srv_nginx01", "srv_docker01"]\n'
        'acl = ["public"]\n'
        "+++\n\n"
        f"{body.strip()}\n",
        encoding="utf-8",
    )


def mutate_database(path: Path, statement: str, parameters: tuple[object, ...] = ()) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA journal_mode = DELETE").fetchone()
        connection.execute(statement, parameters)
        connection.commit()
    finally:
        connection.close()


class KnowledgeCliTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.corpus = self.root / "corpus"
        self.database = self.root / "knowledge.sqlite"
        write_document(
            self.corpus / "nginx" / "502-upstream-timeout.md",
            "Nginx upstream timeout",
            "# Nginx upstream timeout\n\n"
            "## Upstream timeout {#upstream-timeout}\n\n"
            "upstream timed out error.log proxy_read_timeout upstream_response_time\n\n"
            "## Root causes {#root-causes}\n\n"
            "A slow application can exhaust worker threads.",
        )
        write_document(
            self.corpus / "docker" / "oom-kill-diagnostics.md",
            "Docker OOM diagnostics",
            "# Docker OOM diagnostics\n\n"
            "## Triage {#triage}\n\n"
            "OOMKilled cgroup memory.events host OOM memory limit",
        )

    @staticmethod
    def run_cli(*arguments: str) -> tuple[int, str, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            exit_code = cli.main(list(arguments))
        return exit_code, stdout.getvalue(), stderr.getvalue()

    def decode_single_json(self, output: str, *, expected_command: str) -> dict[str, object]:
        self.assertTrue(output.endswith("\n"))
        lines = output.splitlines()
        self.assertEqual(len(lines), 1)
        payload = json.loads(lines[0])
        self.assertIsInstance(payload, dict)
        self.assertEqual(payload["schemaVersion"], 1)
        self.assertEqual(payload["command"], expected_command)
        return payload

    def index(self) -> None:
        exit_code, output, error = self.run_cli(
            "index",
            "--source",
            str(self.corpus),
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        payload = self.decode_single_json(output, expected_command="index")
        self.assertTrue(payload["ok"])

    def assert_error_envelope(
        self,
        payload: dict[str, object],
        *,
        error_code: str,
    ) -> dict[str, object]:
        self.assertFalse(payload["ok"])
        error = payload["error"]
        self.assertIsInstance(error, dict)
        self.assertEqual(error["code"], error_code)
        self.assertIsInstance(error["message"], str)
        self.assertTrue(error["message"])
        return error

    def test_successful_commands_return_zero_and_emit_json(self) -> None:
        exit_code, output, error = self.run_cli(
            "index",
            "--source",
            str(self.corpus),
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        payload = self.decode_single_json(output, expected_command="index")
        self.assertTrue(payload["ok"])
        self.assertIsNone(payload["error"])
        data = payload["data"]
        self.assertIsInstance(data, dict)
        self.assertEqual(data["indexed"], 2)
        self.assertEqual(data["errors"], [])

        exit_code, output, error = self.run_cli(
            "doctor",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        doctor = self.decode_single_json(output, expected_command="doctor")
        self.assertTrue(doctor["ok"])
        self.assertTrue(doctor["data"]["ok"])

        exit_code, output, error = self.run_cli(
            "stats",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        stats = self.decode_single_json(output, expected_command="stats")
        self.assertEqual(stats["data"]["documents"], 2)

        exit_code, output, error = self.run_cli(
            "search",
            "upstream timed out",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        search = self.decode_single_json(output, expected_command="search")
        self.assertTrue(search["ok"])
        self.assertGreater(search["data"]["count"], 0)

        exit_code, output, error = self.run_cli(
            "delete",
            "--db",
            str(self.database),
            "--id",
            "docker/oom-kill-diagnostics.md",
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        deleted = self.decode_single_json(output, expected_command="delete")
        self.assertEqual(deleted["data"]["deleted"], ["docker/oom-kill-diagnostics.md"])

    def test_input_and_storage_errors_use_stable_exit_codes(self) -> None:
        exit_code, output, error = self.run_cli(
            "search",
            "",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (2, ""))
        payload = self.decode_single_json(output, expected_command="search")
        self.assert_error_envelope(payload, error_code="invalid_input")

        missing = self.root / "missing.sqlite"
        exit_code, output, error = self.run_cli(
            "stats",
            "--db",
            str(missing),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (3, ""))
        payload = self.decode_single_json(output, expected_command="stats")
        self.assert_error_envelope(payload, error_code="storage_unavailable")

        foreign = self.root / "foreign.sqlite"
        connection = sqlite3.connect(foreign)
        try:
            connection.execute("PRAGMA application_id = 7")
            connection.execute("CREATE TABLE other(value TEXT)")
            connection.commit()
        finally:
            connection.close()
        exit_code, output, error = self.run_cli(
            "stats",
            "--db",
            str(foreign),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (3, ""))
        payload = self.decode_single_json(output, expected_command="stats")
        self.assert_error_envelope(payload, error_code="storage_unavailable")

    def test_search_beyond_the_token_budget_is_a_stable_input_error(self) -> None:
        self.index()
        oversized = " ".join(chr(0x4E00 + index) for index in range(65))
        exit_code, output, error = self.run_cli(
            "search",
            oversized,
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (2, ""))
        payload = self.decode_single_json(output, expected_command="search")
        failure = self.assert_error_envelope(payload, error_code="invalid_input")
        self.assertEqual(failure["message"], "query has 65 unique searchable tokens; limit is 64")

    def test_corrupt_storage_fails_closed_without_leaking_document_text(self) -> None:
        self.index()
        mutate_database(
            self.database,
            "UPDATE knowledge_chunks SET vector = ?",
            (sqlite3.Binary(b"\x00"),),
        )
        invocations = (
            ("doctor", "--db", str(self.database), "--format", "json"),
            ("search", "upstream timeout", "--db", str(self.database), "--format", "json"),
        )
        for arguments in invocations:
            with self.subTest(command=arguments[0]):
                exit_code, output, error = self.run_cli(*arguments)
                self.assertEqual((exit_code, error), (3, ""))
                payload = self.decode_single_json(
                    output,
                    expected_command=arguments[0],
                )
                self.assert_error_envelope(payload, error_code="storage_unavailable")
                self.assertNotIn("upstream timed out", output)
                self.assertNotIn("OOMKilled", output)
                self.assertNotIn("proxy_read_timeout", output)

    def test_stale_index_returns_storage_error_and_reset_recovers(self) -> None:
        self.index()
        mutate_database(
            self.database,
            "UPDATE knowledge_meta SET value = 'stale' WHERE key = 'chunker_version'",
        )

        exit_code, output, error = self.run_cli(
            "stats",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (3, ""))
        payload = self.decode_single_json(output, expected_command="stats")
        failure = self.assert_error_envelope(payload, error_code="storage_unavailable")
        self.assertIn("stale", failure["message"])
        self.assertIn("index --reset", failure["message"])

        exit_code, output, error = self.run_cli(
            "index",
            "--source",
            str(self.corpus),
            "--db",
            str(self.database),
            "--reset",
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        reset = self.decode_single_json(output, expected_command="index")
        self.assertTrue(reset["ok"])
        self.assertEqual(reset["data"]["indexed"], 2)

        exit_code, output, error = self.run_cli(
            "stats",
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        self.assertTrue(self.decode_single_json(output, expected_command="stats")["ok"])

    def test_search_threshold_failure_uses_a_real_score_and_returns_four(self) -> None:
        self.index()
        query = "upstream timed out proxy_read_timeout"
        exit_code, output, error = self.run_cli(
            "search",
            query,
            "--db",
            str(self.database),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (0, ""))
        baseline = self.decode_single_json(output, expected_command="search")
        scores = [item["score"] for item in baseline["data"]["results"]]
        self.assertTrue(scores)
        threshold = round(max(scores) + 0.01, 8)
        self.assertLessEqual(threshold, 1.0)

        exit_code, output, error = self.run_cli(
            "search",
            query,
            "--db",
            str(self.database),
            "--threshold",
            str(threshold),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (4, ""))
        payload = self.decode_single_json(output, expected_command="search")
        self.assert_error_envelope(payload, error_code="retrieval_result_required")
        self.assertGreater(payload["data"]["rawCount"], 0)
        self.assertEqual(payload["data"]["count"], 0)
        self.assertEqual(payload["data"]["results"], [])
        self.assertEqual(payload["data"]["threshold"], threshold)

    def test_human_output_strips_terminal_bidi_and_zero_width_controls(self) -> None:
        displayed = cli._display(
            "safe\x1b[31mRED\x1b[0m\u202eREVERSED\u202c\u200bZERO\x00CONTROL",
            maximum=200,
        )
        for forbidden in ("\x1b", "\u202e", "\u202c", "\u200b", "\x00"):
            self.assertNotIn(forbidden, displayed)
        self.assertEqual(displayed, "safeREDREVERSEDZEROCONTROL")

    def test_read_commands_open_the_store_read_only(self) -> None:
        self.index()
        with mock.patch.object(cli, "KnowledgeStore", wraps=KnowledgeStore) as tracked:
            exit_code, _, error = self.run_cli("doctor", "--db", str(self.database))
            self.assertEqual((exit_code, error), (0, ""))
            exit_code, _, error = self.run_cli("stats", "--db", str(self.database))
            self.assertEqual((exit_code, error), (0, ""))
            exit_code, _, error = self.run_cli(
                "search",
                "upstream timed out",
                "--db",
                str(self.database),
            )
            self.assertEqual((exit_code, error), (0, ""))

        self.assertEqual(len(tracked.call_args_list), 3)
        for call in tracked.call_args_list:
            self.assertIs(call.kwargs.get("read_only"), True)

    def test_eval_threshold_failure_writes_report_and_returns_four(self) -> None:
        self.index()
        questions = self.root / "questions.jsonl"
        report = self.root / "eval-report.json"
        question_rows = [
            {
                "id": "intentional-recall-miss",
                "question": "upstream timed out proxy_read_timeout",
                "expected_doc_ids": ["docker/oom-kill-diagnostics.md"],
                "expected_section_ids": ["triage"],
                "forbidden_doc_ids": [],
                "filters": {"environment": "demo", "principal": "public"},
            },
            {
                "id": "no-answer",
                "question": "VerticalPodAutoscaler histogram_quantile saturation",
                "expected_doc_ids": [],
                "expected_section_ids": [],
                "forbidden_doc_ids": [],
                "filters": {"environment": "demo", "principal": "public"},
                "expect_no_answer": True,
            },
        ]
        questions.write_text(
            "".join(json.dumps(item, ensure_ascii=False) + "\n" for item in question_rows),
            encoding="utf-8",
        )

        exit_code, output, error = self.run_cli(
            "eval",
            "--db",
            str(self.database),
            "--questions",
            str(questions),
            "--modes",
            "lexical",
            "--k",
            "2",
            "--threshold",
            "0.5",
            "--report",
            str(report),
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (4, ""))
        payload = self.decode_single_json(output, expected_command="eval")
        self.assert_error_envelope(payload, error_code="evaluation_threshold_not_met")
        metrics = payload["data"]["modes"]["lexical"]
        self.assertEqual(metrics["recall"], 0.0)
        self.assertEqual(metrics["noAnswerTotal"], 1)
        self.assertEqual(metrics["noAnswerCorrect"], 1)
        self.assertTrue(report.is_file())
        saved = json.loads(report.read_text(encoding="utf-8"))
        self.assertEqual(saved["modes"]["lexical"]["recall"], 0.0)
        self.assertEqual(saved["noAnswerQuestionCount"], 1)

    def test_eval_threshold_fails_on_forbidden_hits_even_when_recall_is_perfect(self) -> None:
        self.index()
        questions = self.root / "forbidden-questions.jsonl"
        questions.write_text(
            json.dumps({
                "id": "perfect-recall-with-forbidden-hit",
                "question": "upstream timed out OOMKilled cgroup memory limit",
                "expected_doc_ids": ["nginx/502-upstream-timeout.md"],
                "expected_section_ids": ["upstream-timeout"],
                "forbidden_doc_ids": ["docker/oom-kill-diagnostics.md"],
                "filters": {"environment": "demo", "principal": "public"},
            }) + "\n",
            encoding="utf-8",
        )

        exit_code, output, error = self.run_cli(
            "eval",
            "--db",
            str(self.database),
            "--questions",
            str(questions),
            "--modes",
            "lexical",
            "--k",
            "3",
            "--threshold",
            "0.0",
            "--format",
            "json",
        )
        self.assertEqual((exit_code, error), (4, ""))
        payload = self.decode_single_json(output, expected_command="eval")
        failure = self.assert_error_envelope(payload, error_code="evaluation_threshold_not_met")
        self.assertIn("forbidden document hits", failure["message"])
        metrics = payload["data"]["modes"]["lexical"]
        self.assertEqual(metrics["recall"], 1.0)
        self.assertGreater(metrics["forbiddenHitCount"], 0)

    def test_json_error_envelope_strips_body_fields_and_keeps_one_json_line(self) -> None:
        secret = "DO-NOT-LEAK-FULL-DOCUMENT-BODY"
        failure = cli._RetrievalResultError(
            "retrieval failed safely",
            data={
                "query": "safe query",
                "text": secret,
                "Body": secret,
                "nested": [{"content": secret, "safe": "kept"}],
            },
            text=secret,
        )
        with mock.patch.object(cli, "_run", side_effect=failure):
            exit_code, output, error = self.run_cli(
                "search",
                "safe query",
                "--db",
                str(self.database),
                "--format",
                "json",
            )
        self.assertEqual((exit_code, error), (4, ""))
        payload = self.decode_single_json(output, expected_command="search")
        self.assert_error_envelope(payload, error_code="retrieval_result_required")
        self.assertNotIn(secret, output)
        self.assertEqual(payload["data"]["query"], "safe query")
        self.assertEqual(payload["data"]["nested"], [{"safe": "kept"}])

    def test_keyboard_interrupt_returns_one_hundred_thirty(self) -> None:
        with mock.patch.object(cli, "_run", side_effect=KeyboardInterrupt):
            exit_code, output, error = self.run_cli(
                "stats",
                "--db",
                str(self.database),
            )
        self.assertEqual((exit_code, output, error), (130, "", ""))


if __name__ == "__main__":
    unittest.main()
