# Python SSH Service Caller

This package is the Python half of milestone 1. It calls the C++ service over
loopback HTTP and exposes only a structured `service_status` operation. It does
not open SSH connections, construct shell commands, load LLM providers, or run
MCP tools.

## Use

```python
from yukinal_ssh import SshServiceClient

client = SshServiceClient(token="read-the-same-token-from-the-environment")
result = client.service_status("staging-web-01", "nginx.service", timeout_seconds=10)
print(result.stdout.strip(), result.exit_code)
```

Install for local development with `python -m pip install -e agent`. The client
uses only the Python standard library and supports Python 3.11+.

## Tests

```powershell
$env:PYTHONPATH = "agent/src"
python -m unittest discover -s agent/tests -p test_client.py -v
```

The service integration test is added to CTest and launches the built C++
service to check health, allowlist rejection, loopback origin rejection, and
SSH connection error classification. A real SSH success test requires the
optional OpenSSH test container described in the repository README.
