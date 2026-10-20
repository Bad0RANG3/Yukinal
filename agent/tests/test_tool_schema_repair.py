import unittest
from langchain_core.messages import AIMessage
from yukinal_agent import ModelDrivenAgent, ToolCallError
from test_agent_model_loop import ScriptedModel, _registry


def call(i, **extra):
    return AIMessage(content='', tool_calls=[{'name':'ops__read',
        'args':{'target_id':'stage-1', **extra}, 'id':str(i), 'type':'tool_call'}])


class ToolSchemaRepairTests(unittest.IsolatedAsyncioTestCase):
    async def test_invalid_input_returns_to_model_without_executing_then_correct_call_runs_once(self):
        executed=[]
        async def handler(arguments, context):
            executed.append(arguments)
            return {'state':'active'}
        model=ScriptedModel([call(1, max_output_tokens=2000), call(2), AIMessage(content='active')])
        result=await ModelDrivenAgent(_registry(handler),model).run(
            task_id='repair',tenant_id='tenant-1',user_id='user-1',prompt='Check service',
            allowed_tools={'ops.read'},allowed_target_ids={'stage-1'})
        self.assertEqual(result.status,'succeeded')
        self.assertEqual(executed,[{'target_id':'stage-1'}])
        feedback=model.seen_messages[1][-1]
        self.assertEqual(feedback.tool_call_id,'1')
        self.assertIn('not_executed',feedback.content)
        self.assertIn('allowedFields',feedback.content)
        self.assertEqual(result.tool_calls,2)

    async def test_repeated_invalid_input_stops_after_two_repair_opportunities(self):
        executed=[]
        async def handler(arguments, context):
            executed.append(arguments)
        model=ScriptedModel([call(i, invalid=True) for i in range(3)])
        result=await ModelDrivenAgent(_registry(handler),model).run(
            task_id='cap',tenant_id='tenant-1',user_id='user-1',prompt='Check',
            allowed_tools={'ops.read'},allowed_target_ids={'stage-1'})
        self.assertEqual(result.error_code,'invalid_arguments')
        self.assertEqual(len(model.seen_messages),3)
        self.assertFalse(executed)

    async def test_handler_invalid_arguments_is_not_replayed(self):
        executed=[]
        async def handler(arguments, context):
            executed.append(arguments)
            raise ToolCallError('invalid_arguments','Host rejected input after dispatch')
        model=ScriptedModel([call(1),AIMessage(content='must not retry')])
        result=await ModelDrivenAgent(_registry(handler),model).run(
            task_id='host',tenant_id='tenant-1',user_id='user-1',prompt='Check',
            allowed_tools={'ops.read'},allowed_target_ids={'stage-1'})
        self.assertEqual(result.error_code,'invalid_arguments')
        self.assertEqual(len(executed),1)
        self.assertEqual(len(model.seen_messages),1)

    async def test_final_answer_cannot_turn_unrepaired_input_into_success(self):
        executed=[]
        async def handler(arguments, context):
            executed.append(arguments)
        model=ScriptedModel([call(1, invalid=True),AIMessage(content='Done')])
        result=await ModelDrivenAgent(_registry(handler),model).run(
            task_id='unrepaired',tenant_id='tenant-1',user_id='user-1',prompt='Check',
            allowed_tools={'ops.read'},allowed_target_ids={'stage-1'})
        self.assertEqual(result.error_code,'invalid_arguments')
        self.assertEqual(result.status,'failed')
        self.assertFalse(executed)
