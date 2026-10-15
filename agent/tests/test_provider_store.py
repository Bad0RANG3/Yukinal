from __future__ import annotations

import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from yukinal_agent.provider_store import (
    ProviderStore,
    ProviderStoreError,
    validate_provider_save,
)


def _database(path: Path) -> None:
    with closing(sqlite3.connect(path)) as connection:
        connection.executescript(
            """
            CREATE TABLE provider_configs (
                id TEXT PRIMARY KEY,
                family TEXT NOT NULL CHECK (family IN ('ai', 'infra')),
                kind TEXT NOT NULL,
                label TEXT NOT NULL,
                base_url TEXT,
                model TEXT,
                api_key_credential_ref TEXT,
                credential_ref TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                custom_headers TEXT,
                max_input_tokens INTEGER,
                settings TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                wire_api TEXT NOT NULL DEFAULT 'chat',
                api_version TEXT
            );
            """
        )
        connection.commit()


class ProviderStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "yukinal.db"
        _database(self.path)
        self.store = ProviderStore(self.path)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_provider_input_matches_outbound_and_protocol_policy(self) -> None:
        config = validate_provider_save({
            "kind": "openai-compatible",
            "baseUrl": "  http://[::1]:11434/v1/  ",
            "model": " local-model ",
            "wireApi": "responses",
            "customHeaders": {"X-App-Name": " yuki "},
        })
        self.assertEqual(config["baseUrl"], "http://[::1]:11434/v1")
        self.assertEqual(config["model"], "local-model")
        self.assertEqual(config["wireApi"], "responses")
        self.assertEqual(config["customHeaders"], {"X-App-Name": "yuki"})

        invalid_values = (
            {"kind": "openai-compatible", "baseUrl": "http://example.test", "model": "m"},
            {"kind": "gemini", "baseUrl": "https://user:pass@example.test", "model": "m"},
            {"kind": "gemini", "baseUrl": "https://example.test", "model": "m", "wireApi": "chat"},
            {"kind": "openai-compatible", "baseUrl": "https://example.test", "model": "m", "apiVersion": "2026-02-30"},
            {"kind": "openai-compatible", "baseUrl": "https://example.test", "model": "m", "customHeaders": {"Authorization": "secret"}},
            {"kind": "unknown", "baseUrl": "https://example.test", "model": "m"},
        )
        for value in invalid_values:
            with self.subTest(value=value), self.assertRaises(ProviderStoreError):
                validate_provider_save(value)

    def test_save_activates_one_provider_and_preserves_cached_models_and_key_ref(self) -> None:
        config = validate_provider_save({
            "kind": "openai-compatible",
            "baseUrl": "https://gateway.example.test/v1/",
            "model": "model-a",
            "models": [{
                "id": "model-a", "label": "Model A",
                "contextWindow": 4096,
                "supportsToolCalling": True,
                "supportsStreaming": True,
            }],
        })
        first, old_ref = self.store.save_ai("prv_first", config, "keychain://openai/provider_first")
        self.assertIsNone(old_ref)
        self.assertTrue(first["enabled"])
        self.assertEqual(first["apiKeyCredentialRef"], "keychain://openai/provider_first")

        second_config = validate_provider_save({
            "kind": "anthropic",
            "baseUrl": "https://api.anthropic.com/",
            "model": "claude-test",
            "apiVersion": "2024-02-29",
        })
        second, _ = self.store.save_ai("prv_second", second_config, None)
        self.assertTrue(second["enabled"])
        listed = {item["id"]: item for item in self.store.list_ai()}
        self.assertFalse(listed["prv_first"]["enabled"])
        self.assertEqual(listed["prv_first"]["models"][0]["contextWindow"], 4096)
        self.assertEqual(listed["prv_first"]["apiKeyCredentialRef"], "keychain://openai/provider_first")

    def test_activate_and_delete_respect_shared_credential_references(self) -> None:
        first_config = validate_provider_save({
            "kind": "openai-compatible", "baseUrl": "https://one.example/v1", "model": "m1"
        })
        self.store.save_ai("prv_first", first_config, "keychain://openai/shared")
        second_config = validate_provider_save({
            "kind": "openai-compatible", "baseUrl": "https://two.example/v1", "model": "m2"
        })
        self.store.save_ai("prv_second", second_config, "keychain://openai/shared")
        self.assertTrue(self.store.credential_ref_used_elsewhere("keychain://openai/shared", "prv_first"))
        active = self.store.activate_ai("prv_first")
        self.assertTrue(active["enabled"])
        reference, shared = self.store.delete_ai("prv_second")
        self.assertEqual(reference, "keychain://openai/shared")
        self.assertTrue(shared)
        self.assertTrue(self.store.credential_ref_used_elsewhere("keychain://openai/shared", ""))

    def test_delete_reports_an_orphaned_reference_and_unknown_ids_fail(self) -> None:
        config = validate_provider_save({
            "kind": "gemini", "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-test"
        })
        self.store.save_ai("prv_gemini", config, "keychain://openai/provider_gemini")
        reference, shared = self.store.delete_ai("prv_gemini")
        self.assertEqual(reference, "keychain://openai/provider_gemini")
        self.assertFalse(shared)
        with self.assertRaises(ProviderStoreError) as raised:
            self.store.activate_ai("missing")
        self.assertEqual(raised.exception.code, "not_found")

    def test_legacy_kind_spelling_and_malformed_model_cache_are_handled(self) -> None:
        with closing(sqlite3.connect(self.path)) as connection:
            connection.execute(
                "INSERT INTO provider_configs (id, family, kind, label, base_url, model, enabled, "
                "settings, created_at, updated_at) VALUES (?, 'ai', ?, ?, ?, ?, 1, ?, ?, ?)",
                (
                    "prv_legacy", "openaicompatible", "Legacy", "https://example.test/v1", "m",
                    json.dumps({"models": "bad"}), "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z",
                ),
            )
            connection.commit()
        config = self.store.get_ai("prv_legacy")
        self.assertEqual(config["kind"], "openai-compatible")
        self.assertNotIn("models", config)


if __name__ == "__main__":
    unittest.main()
