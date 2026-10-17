"""Bounded, local concurrency proof: no provider, network or server commands."""
from __future__ import annotations

import asyncio
import unittest

from langchain_core.messages import AIMessage, HumanMessage
from yukinal_agent.conversation_store import ConversationStore
from test_agent_conversation_context import LocalSidecar, ScriptedModel, request


class ParallelSessionsTests(unittest.IsolatedAsyncioTestCase):
    async def test_two_sessions_enter_model_together_and_stopping_one_preserves_the_other(self) -> None:
        started = {key: asyncio.Event() for key in ("alpha-only", "beta-only")}
        release = {key: asyncio.Event() for key in started}
        seen = {}

        class WaitingModel(ScriptedModel):
            async def ainvoke(self, messages):
                prompt = next(message.content for message in reversed(messages) if isinstance(message, HumanMessage))
                seen[prompt] = messages
                started[prompt].set()
                await release[prompt].wait()
                return AIMessage(content=prompt + "-answer")

        store = ConversationStore()
        sidecar = LocalSidecar(WaitingModel([]), store)
        first = asyncio.create_task(sidecar.rpc("agent.run.start", request("parallel-a", session="session-a", server="srv_alpha", prompt="alpha-only")))
        second = asyncio.create_task(sidecar.rpc("agent.run.start", request("parallel-b", session="session-b", server="srv_beta", prompt="beta-only")))
        try:
            # This fails under a global execution lock; both must reach the model before either is released.
            await asyncio.wait_for(asyncio.gather(*(event.wait() for event in started.values())), 3)
            self.assertNotIn("beta-only", str(seen["alpha-only"]))
            self.assertNotIn("alpha-only", str(seen["beta-only"]))
            stopped = await sidecar.rpc("agent.run.stop", {"runId": "parallel-b"})
            self.assertEqual(stopped["result"], {"stopped": True})
            self.assertEqual((await asyncio.wait_for(second, 3))["result"]["result"]["state"], "cancelled")
            self.assertFalse(first.done())
            release["alpha-only"].set()
            result = await asyncio.wait_for(first, 3)
            self.assertEqual(result["result"]["result"]["text"], "alpha-only-answer")
            self.assertNotIn("beta-only", str(store.context("session-a", "srv_alpha").messages))
            self.assertNotIn("alpha-only-answer", str(store.context("session-b", "srv_beta").messages))
        finally:
            for event in release.values():
                event.set()
            await sidecar.shutdown()
            await asyncio.gather(first, second, return_exceptions=True)
            store.close()


if __name__ == "__main__":
    unittest.main()
