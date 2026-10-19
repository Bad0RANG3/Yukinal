from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch


_SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "accept_live_python_provider.py"
_SPEC = importlib.util.spec_from_file_location("accept_live_python_provider", _SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
_LIVE_PROVIDER = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_LIVE_PROVIDER)


class LiveProviderAcceptanceConfigTests(unittest.TestCase):
    def test_live_calls_require_explicit_opt_in(self) -> None:
        with patch.dict(os.environ, {}, clear=True), self.assertRaisesRegex(
            ValueError, "YUKINAL_LIVE_PROVIDER_TESTS=1"
        ):
            _LIVE_PROVIDER._live_config()

    def test_openai_provider_uses_a_named_secret_reference(self) -> None:
        with patch.dict(os.environ, {
            "YUKINAL_LIVE_PROVIDER_TESTS": "1",
            "YUKINAL_LIVE_PROVIDER": "openai",
            "YUKINAL_LIVE_MODEL": "tool-model",
            "YUKINAL_LIVE_API_KEY": "test-secret-not-logged",
        }, clear=True):
            provider, config = _LIVE_PROVIDER._live_config()

        self.assertEqual(provider, "openai")
        self.assertEqual(config.model, "tool-model")
        self.assertEqual(config.api_key_env, "YUKINAL_LIVE_API_KEY")
        self.assertEqual(config.max_retries, 0)

    def test_only_loopback_openai_compatible_endpoint_may_omit_api_key(self) -> None:
        common = {
            "YUKINAL_LIVE_PROVIDER_TESTS": "1",
            "YUKINAL_LIVE_PROVIDER": "openai-compatible",
            "YUKINAL_LIVE_MODEL": "local-tool-model",
        }
        with patch.dict(os.environ, {
            **common,
            "YUKINAL_LIVE_BASE_URL": "http://127.0.0.1:1234/v1",
        }, clear=True):
            provider, config = _LIVE_PROVIDER._live_config()
            self.assertEqual(provider, "openai-compatible")
            self.assertEqual(config.base_url, "http://127.0.0.1:1234/v1")

        with patch.dict(os.environ, {
            **common,
            "YUKINAL_LIVE_BASE_URL": "https://provider.example/v1",
        }, clear=True), self.assertRaisesRegex(ValueError, "YUKINAL_LIVE_API_KEY"):
            _LIVE_PROVIDER._live_config()


class LiveProviderAcceptanceHarnessTests(unittest.TestCase):
    def test_local_openai_compatible_endpoint_exercises_the_complete_agent_tool_loop(self) -> None:
        class Handler(BaseHTTPRequestHandler):
            requests: list[dict[str, object]] = []
            protocol_errors: list[str] = []

            def log_message(self, _format: str, *_args: object) -> None:
                return

            def do_POST(self) -> None:
                if self.path != "/v1/chat/completions":
                    self.send_error(404)
                    return
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    request = json.loads(self.rfile.read(length))
                    self.requests.append(request)
                    tool_messages = [message for message in request["messages"] if message.get("role") == "tool"]
                    if tool_messages:
                        tool_content = tool_messages[-1]["content"]
                        token = json.loads(tool_content)["verificationToken"]
                        response_message = {"role": "assistant", "content": token}
                        finish_reason = "stop"
                    else:
                        tools = request.get("tools", [])
                        if not any(
                            tool.get("function", {}).get("name") == "acceptance__probe"
                            for tool in tools
                        ):
                            self.protocol_errors.append("acceptance tool was absent from the provider request")
                        response_message = {
                            "role": "assistant",
                            "content": None,
                            "tool_calls": [{
                                "id": "call_acceptance_probe",
                                "type": "function",
                                "function": {"name": "acceptance__probe", "arguments": "{}"},
                            }],
                        }
                        finish_reason = "tool_calls"
                    response = {
                        "id": "chatcmpl-local-acceptance",
                        "object": "chat.completion",
                        "created": 0,
                        "model": "local-tool-model",
                        "choices": [{"index": 0, "message": response_message, "finish_reason": finish_reason}],
                    }
                    body = json.dumps(response).encode("utf-8")
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except Exception as error:
                    self.protocol_errors.append(type(error).__name__)
                    self.send_error(500)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            environment = os.environ.copy()
            environment.update({
                "YUKINAL_LIVE_PROVIDER_TESTS": "1",
                "YUKINAL_LIVE_PROVIDER": "openai-compatible",
                "YUKINAL_LIVE_MODEL": "local-tool-model",
                "YUKINAL_LIVE_BASE_URL": f"http://127.0.0.1:{server.server_port}/v1",
                "NO_PROXY": "127.0.0.1,localhost,::1",
                "no_proxy": "127.0.0.1,localhost,::1",
            })
            result = subprocess.run(
                [sys.executable, str(_SCRIPT)],
                cwd=Path(__file__).resolve().parents[2],
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
        finally:
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()

        self.assertEqual(result.returncode, 0, msg=result.stderr)
        self.assertIn("PASS live Python Agent tool loop", result.stdout)
        self.assertEqual(len(Handler.requests), 2)
        self.assertEqual(Handler.protocol_errors, [])

    def test_invalid_wire_api_is_rejected_before_any_network_request(self) -> None:
        with patch.dict(os.environ, {
            "YUKINAL_LIVE_PROVIDER_TESTS": "1",
            "YUKINAL_LIVE_PROVIDER": "openai-compatible",
            "YUKINAL_LIVE_MODEL": "tool-model",
            "YUKINAL_LIVE_BASE_URL": "https://provider.example/v1",
            "YUKINAL_LIVE_API_KEY": "test-secret-not-logged",
            "YUKINAL_LIVE_WIRE_API": "unknown",
        }, clear=True), self.assertRaisesRegex(ValueError, "configuration is invalid"):
            _LIVE_PROVIDER._live_config()

    def test_remote_provider_endpoint_requires_https(self) -> None:
        with patch.dict(os.environ, {
            "YUKINAL_LIVE_PROVIDER_TESTS": "1",
            "YUKINAL_LIVE_PROVIDER": "openai-compatible",
            "YUKINAL_LIVE_MODEL": "tool-model",
            "YUKINAL_LIVE_BASE_URL": "http://provider.example/v1",
            "YUKINAL_LIVE_API_KEY": "test-secret-not-logged",
        }, clear=True), self.assertRaisesRegex(ValueError, "must use HTTPS"):
            _LIVE_PROVIDER._live_config()


if __name__ == "__main__":
    unittest.main()
