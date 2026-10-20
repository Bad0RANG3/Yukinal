import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { AGENT_EVENT_MEMBER_SCHEMAS } from "@yukinal/shared";
import { AgentRunStatus } from "../src/features/agent/AgentHeader.js";

const usage = {inputTokens:100,outputTokens:50,reportedRequests:1,unreportedRequests:0,reasoningTokens:30,reasoningReportedRequests:1,
  cacheReadTokens:80,cacheMissTokens:20,cacheReportedRequests:1,
  lastRequest:{durationMs:2400,firstChunkMs:200,firstAnswerMs:900,effort:"low" as const,routing:"auto" as const,model:"deepseek-flash",systemFingerprint:"reported-version",finishReason:"stop"}};
const event = (value: unknown) => ({type:"agent.usage",runId:"run-test",usage:value,at:"2026-10-19T00:00:00Z"});

test("usage boundary accepts legacy counts and bounded reported diagnostics, rejects raw thoughts and invalid counts", () => {
  const schema = AGENT_EVENT_MEMBER_SCHEMAS["agent.usage"];
  assert.equal(schema.safeParse(event({inputTokens:1,outputTokens:2})).success,true);
  assert.equal(schema.safeParse(event(usage)).success,true);
  for (const value of [{...usage,reasoningTokens:-1},{...usage,lastRequest:{...usage.lastRequest,durationMs:Infinity}},
    {...usage,lastRequest:{...usage.lastRequest,reasoning_content:"private"}},{...usage,reasoning_content:"private"}]) {
    assert.equal(schema.safeParse(event(value)).success,false);
  }
});

test("diagnostics show actual effort and answer timing, while absent provider reports stay absent", () => {
  const render = (value: typeof usage|null) => renderToStaticMarkup(<AgentRunStatus running={false} runState="completed" tokenUsage={value} agentReady shell/>);
  const markup = render(usage);
  assert.match(markup,/自动 · 轻量/);
  assert.match(markup,/0\.90/); assert.match(markup,/2\.40/);
  assert.match(markup,/已上报缓存命中/);
  assert.doesNotMatch(render(null),/首段正文|推理用量|缓存命中/);
});
