from __future__ import annotations

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from yukinal_agent.models import ModelConfig
from yukinal_agent.sidecar import _fetch_model_catalog, _map_provider_models


class MapProviderModelsTests(unittest.TestCase):
    """顶层信封校验只认「要用的那个字段」，不认「没用的那些字段」。

    真实网关会在 `data`/`object` 之外加自己的信封键（`success`、`total`、`request_id`），
    这些键不影响解析。旧实现要求顶层键集合恰好等于官方文档那两三个名字，于是「能聊天、
    但获取模型永远失败」。
    """

    def test_openai_compatible_catalog_tolerates_extra_envelope_keys(self) -> None:
        payload = {
            "object": "list",
            "success": True,
            "total": 1,
            "data": [{"id": "deepseek-flash", "object": "model", "owned_by": "gateway"}],
        }
        models = _map_provider_models("openai-compatible", payload)
        self.assertEqual([model["id"] for model in models], ["deepseek-flash"])
        self.assertEqual(models[0]["label"], "deepseek-flash")

    def test_openai_compatible_catalog_still_rejects_a_missing_data_list(self) -> None:
        with self.assertRaises(ValueError):
            _map_provider_models("openai-compatible", {"object": "list"})
        with self.assertRaises(ValueError):
            _map_provider_models("openai-compatible", {"data": "deepseek-flash"})
        with self.assertRaises(ValueError):
            _map_provider_models("openai-compatible", {"data": [{"id": ""}]})
        with self.assertRaises(ValueError):
            _map_provider_models("openai-compatible", {"data": [{"id": 7}]})
        with self.assertRaises(ValueError):
            _map_provider_models("openai-compatible", [{"id": "x"}])

    def test_anthropic_catalog_tolerates_extra_envelope_keys(self) -> None:
        models = _map_provider_models(
            "anthropic",
            {
                "data": [{"id": "claude-x", "display_name": "Claude X"}],
                "has_more": False,
                "first_id": "claude-x",
                "last_id": "claude-x",
                "request_id": "req_1",
            },
        )
        self.assertEqual(models, [{"id": "claude-x", "label": "Claude X", "supportsToolCalling": True, "supportsStreaming": True}])

    def test_gemini_keeps_only_generate_content_models(self) -> None:
        models = _map_provider_models(
            "gemini",
            {
                "models": [
                    {"name": "models/gemini-x", "displayName": "Gemini X", "supportedGenerationMethods": ["generateContent"], "inputTokenLimit": 1234},
                    {"name": "models/embed-y", "displayName": "Embed Y", "supportedGenerationMethods": ["embedContent"]},
                ],
                "nextPageToken": "ignored",
            },
        )
        self.assertEqual([model["id"] for model in models], ["gemini-x"])
        self.assertEqual(models[0]["contextWindow"], 1234)


class FetchModelCatalogTests(unittest.TestCase):
    def test_gateway_envelope_is_accepted_end_to_end(self) -> None:
        class Handler(BaseHTTPRequestHandler):
            authorization: str | None = None

            def log_message(self, _format: str, *_args: object) -> None:
                return

            def do_GET(self) -> None:
                if self.path != "/v1/models":
                    self.send_error(404)
                    return
                Handler.authorization = self.headers.get("Authorization")
                body = json.dumps({
                    "object": "list",
                    "success": True,
                    "data": [{"id": "deepseek-flash", "object": "model"}],
                }).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            config = ModelConfig(
                provider="openai-compatible",
                model="deepseek-flash",
                base_url=f"http://127.0.0.1:{server.server_port}/v1",
                timeout_seconds=5,
                max_retries=0,
            )
            models = _fetch_model_catalog(config, "test-key", 5_000)
        finally:
            server.shutdown()
            server.server_close()
        self.assertEqual([model["id"] for model in models], ["deepseek-flash"])
        self.assertEqual(Handler.authorization, "Bearer test-key")

    def test_a_non_json_catalog_is_reported_as_an_error(self) -> None:
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, _format: str, *_args: object) -> None:
                return

            def do_GET(self) -> None:
                body = b"<html>gateway</html>"
                self.send_response(200)
                self.send_header("Content-Type", "text/html")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            config = ModelConfig(
                provider="openai-compatible",
                model="deepseek-flash",
                base_url=f"http://127.0.0.1:{server.server_port}/v1",
                timeout_seconds=5,
                max_retries=0,
            )
            with self.assertRaises(Exception):
                _fetch_model_catalog(config, None, 5_000)
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
