import unittest
from yukinal_agent import ExecutionContext, MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.host_tools import register_host_tools


class WatchStatusTests(unittest.IsolatedAsyncioTestCase):
    async def test_status_reads_only_current_server_saved_records(self):
        class Host:
            calls = []
            async def request(self, method, params, **kwargs):
                self.calls.append((method, params))
                return {"status": "success", "data": {"serverId": "srv_test", "schedules": [
                    {"status": "active", "lastRunStatus": "failed", "lastError": "inspection_timeout"}]}}
        host = Host()
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)
        context = ExecutionContext(task_id="task_test", tenant_id="local", user_id="user_test",
            allowed_tools=frozenset({"watch.status"}), allowed_target_ids=frozenset({"srv_test"}),
            approved_actions={}, context_target_id="srv_test",
            host_target={"host": "remote", "serverId": "srv_test"})
        result = await registry.execute("watch.status", {}, context, call_id="status_test")
        self.assertEqual(result.value["serverId"], "srv_test")
        self.assertEqual(result.value["schedules"][0], {"status": "已启用", "lastRunStatus": "未完成",
            "lastError": "连接或执行超时，本轮取证未完成"})
        self.assertEqual(host.calls, [("host.context.fetch", {"kind": "watch", "id": "srv_test"})])

    async def test_foreign_status_response_is_rejected(self):
        class Host:
            async def request(self, *args, **kwargs):
                return {"status": "success", "data": {"serverId": "srv_foreign", "schedules": []}}
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, Host())
        context = ExecutionContext(task_id="task_test", tenant_id="local", user_id="user_test",
            allowed_tools=frozenset({"watch.status"}), allowed_target_ids=frozenset({"srv_test"}),
            approved_actions={}, context_target_id="srv_test", host_target={"host": "remote", "serverId": "srv_test"})
        with self.assertRaises(ToolCallError) as caught:
            await registry.execute("watch.status", {}, context, call_id="status_test")
        self.assertEqual(caught.exception.code, "invalid_host_response")
