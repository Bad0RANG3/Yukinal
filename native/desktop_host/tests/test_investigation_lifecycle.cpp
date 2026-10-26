#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/content_revision.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <future>
#include <iostream>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>

#ifdef _WIN32
#include <windows.h>
#endif

namespace {
using nlohmann::json;
using yukinal::desktop_host::DesktopDatabase;
using yukinal::desktop_host::IpcRouter;

std::filesystem::path fixture_wire_path;

void check(bool condition, const char* message) {
  if (!condition) throw std::runtime_error(message);
}

class NoCredentials final : public yukinal::desktop_host::CredentialStore {
 public:
  yukinal::desktop_host::CredentialReference set(
      const std::string&, const std::string&, const yukinal::desktop_host::Secret&) const override {
    throw std::runtime_error("fixture must not write credentials");
  }
  yukinal::desktop_host::Secret get(
      const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("fixture must not read credentials");
  }
  bool has(const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("fixture must not inspect credentials");
  }
  void erase(const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("fixture must not delete credentials");
  }
};

struct TemporaryDirectory {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
      ("yukinal-investigation-lifecycle-" +
       std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
  TemporaryDirectory() { std::filesystem::create_directories(path); }
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};

void set_environment(const char* name, const std::string& value) {
#ifdef _WIN32
  SetEnvironmentVariableA(name, value.c_str());
#else
  setenv(name, value.c_str(), 1);
#endif
}

json task(const std::string& id) {
  return {{"id", id}, {"objective", "Inspect fixture state"},
          {"successCriteria", json::array({"host verifies the saved plan"})},
          {"scope", {{"host", "local"}, {"environment", "local"}}},
          {"guardrails", {{"forbiddenTools", json::array()},
                          {"forbiddenPathPrefixes", json::array()}}},
          {"mode", "goal"}, {"permissionMode", "ask"}, {"automationLevel", "execute"},
          {"phase", "investigating"}, {"status", "pending"},
          {"budget", {{"maxSteps", 10}, {"maxRunMs", 30000}, {"maxAttempts", 10}}},
          {"createdBy", "user"}, {"createdAt", "2026-01-01T00:00:00Z"},
          {"updatedAt", "2026-01-01T00:00:00Z"}};
}

json run(const std::string& id, const std::string& task_id) {
  return {{"id", id}, {"taskId", task_id}, {"attempt", 1},
          {"status", "admitted"}, {"phase", "investigating"},
          {"startedAt", "2026-01-01T00:00:00Z"}, {"updatedAt", "2026-01-01T00:00:00Z"}};
}

json saved_plan(const std::string& id, const std::string& task_id) {
  return {{"id", id}, {"taskId", task_id}, {"revision", 1}, {"status", "active"},
          {"createdAt", "2026-01-01T00:00:01Z"}, {"updatedAt", "2026-01-01T00:00:01Z"},
          {"currentStepId", "fixture_action"}, {"approval", {{"status", "pending"}}},
          {"steps", json::array({{{"id", "fixture_action"}, {"ordinal", 0},
              {"kind", "action"}, {"title", "Review an inert fixture action"},
              {"status", "running"}, {"attempts", 0}, {"maxAttempts", 1},
              {"requiresApproval", true}, {"allowedTools", json::array({"server.exec"})},
              {"evidenceIds", json::array()}}})}};
}

json event(const std::string& type, const std::string& run_id, const std::string& task_id) {
  return {{"type", type}, {"runId", run_id}, {"taskId", task_id},
          {"at", "2026-01-01T00:00:02Z"}};
}

json completion(const std::string& run_id, const std::string& task_id,
                const std::string& state = "completed") {
  auto result = event("agent.completed", run_id, task_id);
  result["result"] = {{"runId", run_id}, {"state", state}, {"text", "fixture finished"},
                       {"steps", 0}, {"toolCalls", 0}};
  return result;
}

void deliver(IpcRouter& router, const json& notification) {
  auto pending = std::async(std::launch::async, [&] {
    return router.invoke("provider_list", {{"events", json::array({notification})}});
  });
  if (pending.wait_for(std::chrono::seconds(3)) != std::future_status::ready) {
    const auto logs = router.invoke("agent_logs", json::object());
    json wire = json::array();
    std::ifstream capture(fixture_wire_path);
    std::string line;
    while (std::getline(capture, line)) {
      const auto parsed = json::parse(line, nullptr, false);
      wire.push_back({{"bytes", line.size()}, {"valid", !parsed.is_discarded()},
                     {"prefix", line.substr(0, 120)}});
    }
    json state = json::object();
    if (notification.contains("taskId") && notification.at("taskId").is_string()) {
      const auto detail = router.invoke("investigation_task_get", {{"taskId", notification.at("taskId")}});
      if (detail.ok) state = detail.value.at("task");
    }
    // Bound a broken fixture without changing any production RPC deadline.
    (void)router.invoke("agent_kill", json::object());
    (void)pending.get();
    throw std::runtime_error("local relay stalled at " + notification.value("type", std::string()) +
        "; logs=" + logs.value.dump() + "; wire=" + wire.dump() + "; task=" + state.dump());
  }
  const auto result = pending.get();
  check(result.ok, "the fixture notification must cross the real sidecar pipe");
}

json read_task(DesktopDatabase& database, const std::string& id) {
  const auto detail = database.get_investigation_task(id);
  check(detail.has_value(), "the fixture task must exist");
  return detail->at("task");
}

std::string start(IpcRouter& router, const std::string& task_id) {
  const auto response = router.invoke("investigation_task_start", {{"taskId", task_id}});
  if (!response.ok || !response.value.value("started", false)) {
    throw std::runtime_error("the fixture run must start: " + response.value.dump());
  }
  return response.value.at("runId").get<std::string>();
}

void create_relay(const std::filesystem::path& directory) {
  std::filesystem::create_directories(directory / "yukinal_agent");
  std::ofstream(directory / "yukinal_agent" / "__init__.py").close();
  fixture_wire_path = directory / "yukinal_agent" / "wire.jsonl";
  std::ofstream script(directory / "yukinal_agent" / "sidecar.py");
  script << R"PY(import json
import sys
from pathlib import Path

# Match the real sidecar pipe contract instead of the Windows locale's GBK
# surrogateescape decoder, which cannot safely echo the native UTF-8 prompt.
sys.stdin.reconfigure(encoding="utf-8", errors="strict")
sys.stdout.reconfigure(encoding="utf-8", errors="strict", newline="\n")
last_start = None

def send(frame):
    payload = json.dumps(frame, separators=(",", ":"))
    with Path(__file__).with_name("wire.jsonl").open("a", encoding="utf-8") as capture:
        capture.write(payload + "\n")
    print(payload, flush=True)

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    params = frame.get("params", {})
    print("fixture received " + str(method), file=sys.stderr, flush=True)
    if method == "initialize":
        result = {"protocolVersion": "1.0", "agentVersion": "lifecycle-fixture"}
    elif method == "tools.list":
        result = {"tools": []}
    elif method == "agent.run.start":
        last_start = params
        send({"jsonrpc": "2.0", "method": "agent.stream", "params": {
            "type": "agent.started", "runId": params["runId"], "taskId": params["taskId"],
            "at": "2026-01-01T00:00:02Z"}})
        result = {"started": True, "runId": params["runId"]}
    elif method == "agent.run.stop":
        result = {"stopped": True}
    elif method == "provider.list":
        for notification in params.get("events", []):
            send({"jsonrpc": "2.0", "method": "agent.stream", "params": notification})
        # Keep an intentional multi-kilobyte response after every event. The
        # lifecycle fixture must still exercise cross-read frame assembly.
        result = {"lastStart": last_start, "fixturePadding": "x" * 5000}
    else:
        send({"jsonrpc": "2.0", "id": frame["id"], "error": {
            "code": -32601, "message": "unsupported lifecycle fixture method"}})
        continue
    send({"jsonrpc": "2.0", "id": frame["id"], "result": result})
)PY";
}

json last_start(IpcRouter& router) {
  const auto response = router.invoke("provider_list", json::object());
  check(response.ok && response.value.contains("lastStart"), "the fixture must capture the actual start request");
  return response.value.at("lastStart");
}

json prompt_state(const json& request) {
  const auto prompt = request.at("prompt").get<std::string>();
  const std::string begin = "\n[host-task-state]\n";
  const std::string end = "\n[/host-task-state]\n";
  const auto offset = prompt.rfind(begin);
  check(offset != std::string::npos, "the actual prompt must include host-owned task state");
  const auto close = prompt.find(end, offset + begin.size());
  check(close != std::string::npos, "the task state must be a complete bounded block");
  const auto encoded = prompt.substr(offset + begin.size(), close - offset - begin.size());
  check(encoded.size() <= 16 * 1024, "the serialized continuation reference must stay within 16 KiB");
  check(prompt.size() <= 64 * 1024, "the actual prompt must respect the Python Agent UTF-8 budget");
  return json::parse(encoded);
}

json remote_task(const std::string& id) {
  auto value = task(id);
  value["serverId"] = "srv_fixture";
  value["scope"] = {{"host", "remote"}, {"serverId", "srv_fixture"}, {"environment", "staging"}};
  value["objective"] = "First turn: save the plan then end. Never delete files or change services.";
  value["guardrails"]["forbiddenTools"] = json::array({"docker.restart"});
  value["guardrails"]["forbiddenPathPrefixes"] = json::array({"/fixture/retain"});
  return value;
}

void run_continuation_prompt_checks(IpcRouter& router, DesktopDatabase& database) {
  const auto original = remote_task("task_prompt_continuation");
  (void)database.create_investigation_task(original);
  const auto first = start(router, "task_prompt_continuation");
  const auto initial = last_start(router);
  const auto initial_state = prompt_state(initial);
  check(initial_state.at("stage") == "initial" && !initial_state.at("actionBindingAttached").get<bool>() &&
            !initial.contains("planBinding") && initial_state.at("plan").is_null(),
        "an initial task must request planning without an invented plan binding");
  check(initial.at("prompt").get<std::string>().find(original.at("objective").get<std::string>()) != std::string::npos,
        "initial planning must preserve the full original objective");
  auto plan = saved_plan("plan_prompt_continuation", "task_prompt_continuation");
  auto& step = plan["steps"][0];
  step["inputBindings"] = {{"command", "id"}};
  step["riskLevel"] = "high";
  step["requiresBaseline"] = true;
  step["preconditions"] = json::array({"retain the exact server and current baseline"});
  step["successCriteria"] = json::array({"the approved command returns uid=0"});
  step["preview"] = "id";
  database.save_investigation_plan(plan);
  const auto saved = *database.latest_investigation_plan("task_prompt_continuation");
  deliver(router, completion(first, "task_prompt_continuation"));
  const auto continued = start(router, "task_prompt_continuation");
  const auto request = last_start(router);
  const auto state = prompt_state(request);
  const auto prompt = request.at("prompt").get<std::string>();
  check(state.at("stage") == "continuation" && state.at("currentStepRunnable") == true &&
            state.at("plan").at("approvalStatus") == "pending" && state.at("actionBindingAttached") == true,
        "an unapproved saved plan must be described as continuation without inventing approval");
  check(state.at("plan").at("id") == request.at("planBinding").at("planId") &&
            state.at("currentStep").at("id") == request.at("planBinding").at("planStepId") &&
            state.at("currentStep").at("evidenceIds") == request.at("planBinding").at("evidenceIds") &&
            state.at("currentStep").at("inputBindings") == step.at("inputBindings") &&
            state.at("currentStep").at("allowedTools") == step.at("allowedTools"),
        "the actual prompt and action binding must describe the same host-read plan step");
  check(state.at("currentStep").at("requiresApproval") == true &&
            state.at("currentStep").at("requiresBaseline") == true &&
            state.at("mode") == request.at("mode") && state.at("permissionMode") == request.at("permissionMode") &&
            state.at("automationLevel") == original.at("automationLevel") &&
            prompt.find(original.at("objective").get<std::string>()) != std::string::npos &&
            prompt.find("docker.restart") != std::string::npos && prompt.find("/fixture/retain") != std::string::npos &&
            prompt.find("未明确限定为首轮的禁令") != std::string::npos &&
            prompt.find("此续跑说明不会取消原目标的通用禁止") != std::string::npos &&
            prompt.find("提交一次精确工具调用来创建真实审批请求") != std::string::npos &&
            prompt.find("只有真实批准并通过宿主复核后才执行") != std::string::npos &&
            prompt.find("不得伪造 approvalId") != std::string::npos &&
            *database.latest_investigation_plan("task_prompt_continuation") == saved,
        "continuation must preserve the original objective, hard boundaries, approval, baseline and saved revision");
  check(prompt != initial.at("prompt").get<std::string>(),
        "a second start must not resend the identical first-turn planning prompt");
  deliver(router, completion(continued, "task_prompt_continuation"));

  for (const auto* variant : {"missing_current", "unknown_current", "completed_plan", "exhausted_step", "blocked_step"}) {
    const std::string id = std::string("task_prompt_") + variant;
    (void)database.create_investigation_task(remote_task(id));
    auto inactive = saved_plan("plan_" + id, id);
    const std::string name = variant;
    if (name == "missing_current") inactive.erase("currentStepId");
    if (name == "unknown_current") inactive["currentStepId"] = "missing_step";
    if (name == "completed_plan") inactive["status"] = "completed";
    if (name == "exhausted_step") inactive["steps"][0]["attempts"] = 1;
    if (name == "blocked_step") inactive["steps"][0]["status"] = "blocked";
    database.save_investigation_plan(inactive);
    const auto active = start(router, id);
    const auto blocked_request = last_start(router);
    const auto blocked = prompt_state(blocked_request);
    check(blocked.at("stage") == "blocked" && blocked.at("currentStepRunnable") == false,
          "missing, completed, blocked or exhausted current steps must not receive a continue-action instruction");
    if (name == "missing_current" || name == "unknown_current" || name == "completed_plan") {
      check(!blocked_request.contains("planBinding"), "an unavailable host action binding must not be fabricated");
    }
    deliver(router, completion(active, id));
  }

  const std::string large_id = "task_prompt_truncated";
  (void)database.create_investigation_task(remote_task(large_id));
  auto large = saved_plan("plan_prompt_truncated", large_id);
  std::string unicode;
  for (int index = 0; index < 2048; ++index) unicode += "\xe7\x95\x8c";
  large["steps"][0]["title"] = unicode;
  large["steps"][0]["inputBindings"] = {{"command", unicode}, {"purpose", unicode}, {"workdir", unicode}};
  database.save_investigation_plan(large);
  const auto large_run = start(router, large_id);
  const auto large_request = last_start(router);
  const auto truncated = prompt_state(large_request);
  check(truncated.at("stage") == "blocked" && truncated.at("referenceTruncated") == true &&
            truncated.at("currentStepRunnable") == false &&
            !truncated.at("currentStep").contains("inputBindings") &&
            !truncated.at("currentStep").contains("allowedTools") &&
            truncated.at("currentStep").at("title").get<std::string>().size() == 256 * 3 &&
            large_request.at("planBinding").at("planId") == "plan_prompt_truncated" &&
            large_request.at("prompt").get<std::string>().find(remote_task(large_id).at("objective").get<std::string>()) != std::string::npos &&
            database.latest_investigation_plan(large_id)->at("steps")[0].at("inputBindings") == large["steps"][0].at("inputBindings"),
        "oversized UTF-8 reference must omit action details as a whole, retain valid identity and preserve exact durable bindings");
  deliver(router, completion(large_run, large_id));

  // Legacy rows can contain oversized identity fields too. The final fallback
  // still publishes valid bounded JSON rather than an apparently exact prefix.
  const std::string legacy_id = "task_prompt_legacy_oversize";
  (void)database.create_investigation_task(remote_task(legacy_id));
  auto legacy = saved_plan("plan_prompt_legacy_oversize", legacy_id);
  legacy["steps"][0]["riskLevel"] = std::string(20000, 'x');
  database.save_investigation_plan(legacy);
  const auto legacy_run = start(router, legacy_id);
  const auto legacy_state = prompt_state(last_start(router));
  check(legacy_state.at("stage") == "blocked" && legacy_state.at("referenceTruncated") == true &&
            legacy_state.at("plan").is_null() && legacy_state.at("currentStep").is_null(),
        "oversized legacy metadata must also stay within the whole-reference budget");
  deliver(router, completion(legacy_run, legacy_id));

  for (const bool has_plan : {false, true}) {
    const std::string recovery_id = has_plan ? "task_prompt_recovery" : "task_prompt_recovery_no_plan";
    auto recovered = remote_task(recovery_id);
    recovered["lastFailure"] = {{"code", "outcome_unknown"}, {"message", "inert fixture outcome unknown"},
        {"retryable", false}, {"attempt", 1}, {"at", "2026-01-01T00:00:02Z"},
        {"detail", {{"requiresFreshBaseline", true}}}};
    (void)database.create_investigation_task(recovered);
    if (has_plan) {
      const auto plan = saved_plan("plan_prompt_recovery", recovery_id);
      database.save_investigation_plan(plan);
      const auto previous_start = last_start(router);
      const auto blocked = router.invoke("investigation_task_start", {{"taskId", recovery_id}});
      check(!blocked.ok && blocked.value.at("code") == "INVALID_STATE" &&
                blocked.value.at("message").get<std::string>().find("恢复已暂停") != std::string::npos &&
                database.get_investigation_task(recovery_id)->at("runs").empty() &&
                !read_task(database, recovery_id).contains("activeRunId") &&
                *database.latest_investigation_plan(recovery_id) == plan &&
                last_start(router) == previous_start,
            "a fresh-baseline/action conflict must pause before model admission without an attempt or changed permissions");
      continue;
    }
    const auto recovery_run = start(router, recovery_id);
    const auto recovery_request = last_start(router);
    const auto recovery_prompt = recovery_request.at("prompt").get<std::string>();
    check(prompt_state(recovery_request).at("stage") == "recovery" &&
              recovery_prompt.find("上一轮远端命令或变更的结果未知") != std::string::npos &&
              recovery_prompt.find("证据明确前禁止重放、补发或改写同一行动") != std::string::npos &&
              recovery_prompt.find("规划和结束要求已经完成") == std::string::npos &&
              recovery_prompt.find(recovered.at("objective").get<std::string>()) != std::string::npos,
          "existing recovery restrictions must retain priority, including when no plan exists");
    deliver(router, completion(recovery_run, recovery_id));
  }

  const std::string overflow_id = "task_prompt_overflow";
  auto overflow = remote_task(overflow_id);
  std::string large_objective;
  for (int index = 0; index < 8192; ++index) large_objective += "\xe7\x95\x8c";
  std::string criterion;
  for (int index = 0; index < 1024; ++index) criterion += "\xe7\x95\x8c";
  overflow["objective"] = large_objective;
  overflow["successCriteria"] = json::array();
  for (int index = 0; index < 16; ++index) overflow["successCriteria"].push_back(criterion);
  (void)database.create_investigation_task(overflow);
  const auto rejected = router.invoke("investigation_task_start", {{"taskId", overflow_id}});
  check(!rejected.ok && rejected.value.at("code") == "INVALID_STATE" &&
            !read_task(database, overflow_id).contains("activeRunId") &&
            database.get_investigation_task(overflow_id)->at("runs").empty() &&
            read_task(database, overflow_id).at("objective") == large_objective,
        "an over-budget original prompt must fail before admission without truncating its objective or consuming an attempt");
}

void run_stage_progress_checks(IpcRouter& router, DesktopDatabase& database) {
  // The Agent's derived stage is an observation of durable progress. It must move
  // the task phase only for the run that owns the task, never past a terminal
  // task, and never from a stage the host does not understand.
  const std::string id = "task_stage_progress";
  (void)database.create_investigation_task(remote_task(id));
  const auto active = start(router, id);
  const auto stage = [&](const std::string& value, const std::string& run) {
    auto notification = event("agent.stage", run, id);
    notification["stage"] = value;
    notification["previous"] = "intake";
    return notification;
  };
  check(read_task(database, id).at("phase") == "investigating",
        "a started task begins investigating before any stage is derived");
  deliver(router, stage("evidence", active));
  check(read_task(database, id).at("phase") == "investigating",
        "collecting evidence keeps the investigating phase");
  deliver(router, stage("change", active));
  check(read_task(database, id).at("phase") == "execution",
        "an executed change must move the task into execution");
  deliver(router, stage("verify", active));
  check(read_task(database, id).at("phase") == "verification" &&
            database.get_investigation_run(active)->at("phase") == "verification",
        "verification must be visible on both the task and its run");
  const auto before = read_task(database, id);
  deliver(router, stage("unmodelled_stage", active));
  check(read_task(database, id) == before,
        "an unknown stage must be ignored rather than inventing a phase");
  // A foreign run cannot move the phase of a task it does not own.
  deliver(router, stage("diagnosis", "run_not_owning_this_task"));
  check(read_task(database, id) == before,
        "a stage from a non-owning run must not change the task phase");
  deliver(router, completion(active, id));
  const auto ended = read_task(database, id);
  deliver(router, stage("investigating", active));
  check(read_task(database, id) == ended,
        "a terminal task must not be revived by a late stage event");
  check(ended.at("status") == "waiting_user",
        "a completed model attempt still leaves plan verification to the host");
}

void run_interrupted_plan_checks(IpcRouter& router, DesktopDatabase& database) {  const auto failure_at = "2026-01-01T00:00:05Z";
  const auto prepare = [&](const std::string& id) {
    (void)database.create_investigation_task(remote_task(id));
    database.save_investigation_plan(saved_plan("plan_" + id, id));
    return start(router, id);
  };
  const auto fail = [&](const std::string& id, const std::string& run_id,
                        const std::string& error = "SSH authentication timed out") {
    auto notification = event("agent.failed", run_id, id);
    notification["error"] = error;
    deliver(router, notification);
  };
  const auto accept_failure = [&](const std::string& id, const std::string& run_id) {
    check(database.apply_investigation_run_progress(id, run_id, "failed",
        json{{"code", "authentication"}, {"message", "SSH authentication timed out"},
             {"retryable", false}, {"attempt", 1}, {"at", failure_at}}, failure_at).has_value(),
        "the fixture failure must be an accepted terminal run");
  };

  for (const auto* status : {"failed", "cancelled"}) {
    const auto id = std::string("task_interrupted_") + status;
    const auto active = prepare(id);
    const auto before = *database.latest_investigation_plan(id);
    if (std::string(status) == "failed") fail(id, active);
    else deliver(router, completion(active, id, "cancelled"));
    const auto after = *database.latest_investigation_plan(id);
    const auto& step = after.at("steps").at(0);
    check(step.at("status") == "blocked" && step.at("attempts") == 0 && step.contains("endedAt") &&
              after.at("revision") == before.at("revision") &&
              after.at("approval") == before.at("approval") &&
              step.at("allowedTools") == before.at("steps").at(0).at("allowedTools") &&
              !step.contains("outputSummary") &&
              step.at("lastDeviation").at("message").get<std::string>().find("不表示命令已执行或成功") != std::string::npos &&
              database.get_investigation_task(id)->at("steps").empty(),
          "a failed bound run must block its unchanged step without inventing execution, attempts or output");
    if (std::string(status) == "failed") {
      check(step.at("lastDeviation").at("message").get<std::string>().find("SSH authentication timed out") != std::string::npos &&
                database.get_investigation_run(active)->at("failure").at("message") == "SSH authentication timed out",
            "the actual authentication failure must survive conservative interruption recording");
    }
    fail(id, active, "late unrelated error");
    check(*database.latest_investigation_plan(id) == after &&
              !database.block_interrupted_investigation_plan(id, active,
                  database.get_investigation_run(active)->at("endedAt").get<std::string>()),
          "duplicate and replayed terminal events must not settle a step twice");
  }

  const auto denied_id = "task_interrupted_denied";
  const auto denied_run = prepare(denied_id);
  const auto denied_before = *database.latest_investigation_plan(denied_id);
  const auto denied = router.handle_host_request("host.tool.execute", {
      {"callId", "call_fixture_denied"}, {"traceId", "trace_fixture_denied"},
      {"runId", denied_run}, {"taskId", denied_id}, {"toolName", "server.info"},
      {"input", json::object()}, {"target", remote_task(denied_id).at("scope")},
      {"planId", denied_before.at("id")}, {"planStepId", "fixture_action"},
      {"evidenceIds", json::array()}});
  check(denied.at("status") == "failed" && denied.at("error").at("code") == "plan_deviation" &&
            *database.latest_investigation_plan(denied_id) == denied_before &&
            !database.host_tool_call_status("trace_fixture_denied", "call_fixture_denied").has_value(),
        "an off-plan tool must be rejected before execution or call-ledger admission");
  fail(denied_id, denied_run, "plan_deviation");
  check(database.latest_investigation_plan(denied_id)->at("steps").at(0).at("attempts") == 0,
        "an off-plan server.info denial must never count as the bound server.exec execution");

  // A successful Agent turn is not a trusted successful tool receipt. Keep its
  // established waiting/planning semantics unchanged in this bounded repair.
  const auto completed_run = prepare("task_interrupted_completed");
  const auto completed_before = *database.latest_investigation_plan("task_interrupted_completed");
  deliver(router, completion(completed_run, "task_interrupted_completed"));
  check(*database.latest_investigation_plan("task_interrupted_completed") == completed_before,
        "normal completed turns must not be guessed into tool success or interruption");

  for (const auto* variant : {"new_id", "new_revision", "new_current", "changed_input",
                             "wrong_session", "wrong_checkpoint", "legacy", "pending", "succeeded"}) {
    const auto id = std::string("task_interrupted_guard_") + variant;
    const auto active = prepare(id);
    const std::string name = variant;
    auto plan = *database.latest_investigation_plan(id);
    if (name == "new_id") { plan["id"] = "replacement_" + id; plan["revision"] = 2; }
    if (name == "new_revision") plan["revision"] = 2;
    if (name == "new_current") {
      auto next = plan["steps"][0]; next["id"] = "replacement_step";
      plan["steps"].push_back(next); plan["currentStepId"] = "replacement_step";
    }
    if (name == "changed_input") plan["steps"][0]["inputBindings"] = {{"command", "whoami"}};
    if (name == "pending" || name == "succeeded") plan["steps"][0]["status"] = name;
    database.save_investigation_plan(plan);
    if (name == "wrong_session" || name == "wrong_checkpoint" || name == "legacy") {
      auto run = *database.get_investigation_run(active);
      if (name == "wrong_session") run["sessionId"] = "session_foreign";
      if (name == "wrong_checkpoint") run["checkpoint"]["hostPlanStep"]["taskId"] = "task_foreign";
      if (name == "legacy") run.erase("checkpoint");
      check(database.update_investigation_run(run).has_value(), "the inert run mutation must persist");
    }
    fail(id, active);
    check(*database.latest_investigation_plan(id) == plan,
          "stale revision/current step, changed intent, foreign session, legacy and settled steps must remain untouched");
  }

  // Simulate the gap between accepting a failure and its plan CAS: even a newer
  // run that has already ended must fence out the older interruption marker.
  for (const bool finish_newer : {false, true}) {
    const auto id = finish_newer ? "task_interrupted_newer_ended" : "task_interrupted_newer_active";
    const auto old = prepare(id);
    const auto before = *database.latest_investigation_plan(id);
    accept_failure(id, old);
    auto newer = *database.get_investigation_run(old);
    newer["id"] = std::string("run_newer_") + id; newer["attempt"] = 2;
    newer["status"] = "running"; newer.erase("endedAt"); newer.erase("failure");
    database.create_investigation_run(newer);
    (void)database.update_investigation_task_progress(id, "investigating", "investigating",
        newer.at("id").get<std::string>(), std::nullopt, failure_at, std::nullopt);
    if (finish_newer) accept_failure(id, newer.at("id").get<std::string>());
    check(!database.block_interrupted_investigation_plan(id, old, failure_at) &&
              *database.latest_investigation_plan(id) == before,
          "a newer active or already-terminal run must fence out the old plan CAS");
  }

  for (int index = 0; index < 10; ++index) {
    const auto id = "task_interrupted_revision_race_" + std::to_string(index);
    const auto old = prepare(id);
    accept_failure(id, old);
    auto newer = *database.latest_investigation_plan(id);
    newer["revision"] = 2;
    newer["steps"][0]["inputBindings"] = {{"command", "id -u"}};
    std::atomic<bool> ready{false};
    std::thread mark([&] {
      while (!ready.load()) std::this_thread::yield();
      (void)database.block_interrupted_investigation_plan(id, old, failure_at);
    });
    std::thread replace([&] {
      while (!ready.load()) std::this_thread::yield();
      database.save_investigation_plan(newer);
    });
    ready.store(true); mark.join(); replace.join();
    check(*database.latest_investigation_plan(id) == newer,
          "a transaction race must never overwrite the replacement revision with the old step snapshot");
  }

  for (const auto* recovery : {"fresh_baseline", "rollback", "outcome_unknown"}) {
    const auto id = std::string("task_recovery_conflict_") + recovery;
    auto value = remote_task(id);
    value["lastFailure"] = {{"code", std::string(recovery) == "outcome_unknown" ? "outcome_unknown" : "authentication"},
        {"message", "fixture recovery"}, {"retryable", false}, {"attempt", 1},
        {"at", failure_at}, {"detail", {{"requiresFreshBaseline", true},
                                      {"rollbackRequested", std::string(recovery) == "rollback"}}}};
    (void)database.create_investigation_task(value);
    const auto before = saved_plan("plan_" + id, id);
    database.save_investigation_plan(before);
    const auto response = router.invoke("investigation_task_start", {{"taskId", id}});
    check(!response.ok && response.value.at("code") == "INVALID_STATE" &&
              *database.latest_investigation_plan(id) == before &&
              database.get_investigation_task(id)->at("runs").empty() &&
              read_task(database, id).at("lastFailure") == value.at("lastFailure"),
          "fresh baseline, unknown outcome and rollback must pause without changing permissions, failure or budget");
  }

  const auto readonly_id = "task_recovery_bound_evidence";
  auto readonly = remote_task(readonly_id);
  readonly["lastFailure"] = {{"code", "authentication"}, {"detail", {{"requiresFreshBaseline", true}}}};
  (void)database.create_investigation_task(readonly);
  auto evidence_plan = saved_plan("plan_recovery_bound_evidence", readonly_id);
  evidence_plan["steps"][0]["kind"] = "evidence";
  evidence_plan["steps"][0]["allowedTools"] = json::array({"server.info"});
  evidence_plan["steps"][0]["requiresApproval"] = false;
  database.save_investigation_plan(evidence_plan);
  const auto readonly_run = start(router, readonly_id);
  const auto request = last_start(router);
  check(prompt_state(request).at("stage") == "recovery" && !request.contains("planBinding") &&
            prompt_state(request).at("currentStep").at("allowedTools") == json::array({"server.info"}) &&
            request.at("prompt").get<std::string>().find("只运行宿主当前步骤允许的工具并精确遵守 inputBindings") != std::string::npos &&
            *database.latest_investigation_plan(readonly_id) == evidence_plan,
        "an already permitted read-only recovery step may continue without inventing an action binding or broader tools");
  deliver(router, completion(readonly_run, readonly_id));
}

void run_recovery_state_checks(IpcRouter& router, DesktopDatabase& database) {
  const std::string now = "2026-01-01T00:00:10Z";
  for (const auto* action : {"retry", "resume"}) {
    const auto id = std::string("task_recovery_waiting_") + action;
    auto value = remote_task(id);
    const bool resume = std::string(action) == "resume";
    value["status"] = resume ? "stopped" : "failed";
    value["phase"] = "recovery";
    value["lastFailure"] = {{"code", resume ? "cancelled" : "authentication"},
        {"message", "SSH authentication timed out"}, {"retryable", false},
        {"attempt", 2}, {"at", now},
        {"options", json::array({{{"id", action}, {"action", action}, {"title", "fixture recovery"},
            {"description", "fixture recovery"}, {"requiresApproval", false}}})}};
    (void)database.create_investigation_task(value);
    const auto plan = saved_plan("plan_" + id, id);
    database.save_investigation_plan(plan);
    for (int attempt = 1; attempt <= 2; ++attempt) {
      auto historic = run("run_" + id + "_" + std::to_string(attempt), id);
      historic["attempt"] = attempt;
      historic["status"] = resume ? "cancelled" : "failed";
      historic["endedAt"] = now;
      historic["failure"] = value.at("lastFailure");
      database.create_investigation_run(historic);
    }
    const auto historic = database.list_investigation_runs(id, 10);
    const auto previous_start = last_start(router);
    const auto recovered = router.invoke("investigation_task_recover", {{"input", {
        {"taskId", id}, {"optionId", action}}}});
    const auto paused = read_task(database, id);
    const auto after = *database.latest_investigation_plan(id);
    check(recovered.ok && recovered.value.at("task") == paused &&
              paused.at("status") == "waiting_user" && paused.at("phase") == "recovery" &&
              !paused.contains("activeRunId") &&
              paused.at("lastFailure").at("message").get<std::string>().find("请先审阅新的计划") != std::string::npos &&
              paused.at("lastFailure").at("message").get<std::string>().find("尚未启动新运行") != std::string::npos &&
              database.list_investigation_runs(id, 10) == historic && historic.size() == 2 &&
              after.at("revision") == 1 && after.at("steps") == plan.at("steps") &&
              after.at("steps").at(0).at("attempts") == 0 && last_start(router) == previous_start,
          "conflicting retry/resume must atomically wait with a visible reason, unchanged old runs and zero executions");
    const auto blocked = router.invoke("investigation_task_start", {{"taskId", id}});
    check(!blocked.ok && blocked.value.at("code") == "INVALID_STATE" &&
              read_task(database, id) == paused && !paused.contains("activeRunId") &&
              database.list_investigation_runs(id, 10) == historic && last_start(router) == previous_start,
          "recover followed by guard rejection must stay waiting without a new run or model request");
  }

  // Preserve the existing explicit UI auto-start path for admissible recovery.
  // Neither a missing initial plan nor an already-permitted evidence step is
  // turned into an inert waiting task by the conflict-specific state repair.
  for (const bool has_plan : {false, true}) {
    const auto id = has_plan ? "task_recovery_state_readonly" : "task_recovery_state_no_plan";
    auto value = remote_task(id);
    value["status"] = "stopped";
    value["lastFailure"] = {{"code", "cancelled"}, {"message", "fixture cancelled"},
        {"retryable", false}, {"attempt", 1}, {"at", now}};
    (void)database.create_investigation_task(value);
    if (has_plan) {
      auto plan = saved_plan("plan_" + std::string(id), id);
      plan["steps"][0]["kind"] = "evidence";
      plan["steps"][0]["allowedTools"] = json::array({"server.info"});
      plan["steps"][0]["requiresApproval"] = false;
      database.save_investigation_plan(plan);
    }
    const auto recovered = router.invoke("investigation_task_recover", {{"input", {{"taskId", id}}}});
    check(recovered.ok && recovered.value.at("task").at("status") == "investigating" &&
              !recovered.value.at("task").contains("activeRunId") &&
              database.get_investigation_task(id)->at("runs").empty(),
          "an admissible recovery must retain the frontend's existing explicit auto-start condition");
    const auto active = start(router, id);
    check(read_task(database, id).at("status") == "investigating" &&
              read_task(database, id).at("activeRunId") == active &&
              database.get_investigation_run(active)->at("status") == "running" &&
              database.get_investigation_task(id)->at("runs").size() == 1,
          "normal recovery must admit exactly one real run through the existing start entry point");
    deliver(router, completion(active, id));
  }

  const auto changed_id = "task_recovery_state_changed_plan";
  auto changed_task = remote_task(changed_id);
  changed_task["status"] = "stopped";
  (void)database.create_investigation_task(changed_task);
  auto expected = saved_plan("plan_recovery_state_changed_plan", changed_id);
  expected["steps"][0]["kind"] = "evidence";
  expected["steps"][0]["allowedTools"] = json::array({"server.info"});
  database.save_investigation_plan(expected);
  const auto fingerprint = yukinal::desktop_host::content_revision_hex(
      database.latest_investigation_plan(changed_id)->dump());
  auto replacement = expected;
  replacement["revision"] = 2;
  database.save_investigation_plan(replacement);
  const auto changed = database.recover_investigation_task(changed_id, now,
      json{{"code", "cancelled"}, {"message", "fixture cancelled"},
           {"detail", {{"requiresFreshBaseline", true}}}}, false, fingerprint);
  check(changed.has_value() && changed->at("status") == "waiting_user" &&
            changed->at("lastFailure").at("message").get<std::string>().find("恢复期间当前计划发生变化") != std::string::npos &&
            !changed->contains("activeRunId") && *database.latest_investigation_plan(changed_id) == replacement &&
            database.get_investigation_task(changed_id)->at("runs").empty(),
        "the recovery transaction must repeat the latest-plan precheck and wait if its snapshot changed");
}

void run_tests(const std::string& python) {
  TemporaryDirectory directory;
  const auto relay = directory.path / "relay";
  create_relay(relay);
  set_environment("YUKINAL_AGENT_ENTRY", "");
  set_environment("YUKINAL_AGENT_PYTHON", python);
  set_environment("YUKINAL_AGENT_SOURCE", relay.string());
  DesktopDatabase database(directory.path / "data");
  NoCredentials credentials;
  IpcRouter router(&database, &credentials);
  const auto spawned = router.invoke("agent_spawn", json::object());
  check(spawned.ok && spawned.value.value("agentVersion", std::string()) == "lifecycle-fixture",
        "only the local lifecycle fixture may be started");
  // No UI sink is installed: persistence cannot depend on an open WebView.

  // Saving a plan completes the attempt, not the task. A new trusted start
  // reads that plan back and carries its binding, without executing its action.
  const json server = {{"id", "srv_fixture"}, {"name", "Inert fixture"},
      {"connection", {{"host", "127.0.0.1"}, {"port", 1}, {"username", "fixture"}}},
      {"metadata", {{"environment", "staging"}}}, {"capabilities", json::object()},
      {"status", "disconnected"}, {"createdAt", "2026-01-01T00:00:00Z"},
      {"updatedAt", "2026-01-01T00:00:00Z"}};
  database.add_server(server, std::nullopt, "activity_fixture");
  // Drive the real native -> sidecar async admission -> native event/SQLite path.
  auto watch_task = remote_task("task_watch_relay");
  watch_task["mode"] = "readonly";
  watch_task["automationLevel"] = "readonly";
  (void)database.create_investigation_task(watch_task);
  const auto watch = router.invoke("investigation_schedule_create", {{"input", {
      {"id", "watch_relay"}, {"taskId", "task_watch_relay"}, {"intervalSeconds", 30},
      {"cooldownSeconds", 0}, {"dedupeWindowSeconds", 1}, {"notificationPolicy", "on_change"}}}});
  check(watch.ok && router.poll_schedules().ok, "watch clock must run without a mounted UI");
  const auto watch_start = last_start(router);
  check(watch_start["delivery"] == "async" && watch_start["resume"] == true &&
            watch_start["mode"] == "readonly" && watch_start["target"]["serverId"] == "srv_fixture",
        "wire admission must retain scoped readonly async execution");
  const auto watch_run = watch_start.at("runId").get<std::string>();
  auto watch_tool = event("agent.tool_result", watch_run, "task_watch_relay");
  watch_tool["toolName"] = "server.logs"; watch_tool["status"] = "success";
  deliver(router, watch_tool);
  auto watch_done = completion(watch_run, "task_watch_relay");
  watch_done["result"]["text"] = R"({"outcome":"changed","summary":"Fixture login failures detected"})";
  deliver(router, watch_done);
  check(database.get_investigation_schedule_run(watch_run)->at("status") == "succeeded" &&
            database.get_investigation_task("task_watch_relay")->at("task").at("status") == "pending",
        "wire completion must finish the inspection without terminating its persistent task");
  check(router.invoke("investigation_schedule_update", {{"input", {
      {"scheduleId", "watch_relay"}, {"status", "paused"}}}}).ok, "fixture watch must pause");
  auto planned = task("task_planned");
  planned["serverId"] = "srv_fixture";
  planned["scope"] = {{"host", "remote"}, {"serverId", "srv_fixture"}, {"environment", "staging"}};
  (void)database.create_investigation_task(planned);
  const auto first = start(router, "task_planned");
  check(database.get_investigation_run(first)->at("status") == "running",
        "agent.started must persist a running attempt");
  database.save_investigation_plan(saved_plan("plan_fixture", "task_planned"));
  deliver(router, completion(first, "task_planned"));
  const auto after_plan = read_task(database, "task_planned");
  check(after_plan.at("status") == "waiting_user" && after_plan.at("phase") == "decision" &&
            !after_plan.contains("activeRunId") && !after_plan.contains("completedAt") &&
            database.get_investigation_run(first)->at("status") == "completed" &&
            database.get_investigation_run(first)->contains("endedAt"),
        "a completed planning attempt must release the task for its next turn");
  check(database.latest_investigation_plan("task_planned")->at("approval").at("status") ==
            "pending", "an Agent completion must not approve its saved plan");
  database.record_investigation_brief({{"id", "brief_fixture"}, {"taskId", "task_planned"},
      {"planId", "plan_fixture"}, {"generatedAt", "2026-01-01T00:00:02Z"},
      {"status", "draft"}, {"findingIds", json::array()},
      {"options", json::array({{{"id", "option_start"}, {"title", "Start reviewed plan"},
          {"summary", "Continue the inert fixture"}, {"impact", "No real tool executes"},
          {"riskLevel", "medium"}, {"evidenceIds", json::array()},
          {"findingIds", json::array()}, {"verification", "fixture assertions"},
          {"requiresApproval", true}, {"status", "proposed"}, {"continuation", "start_plan"}}})}});
  const auto choice = router.invoke("investigation_brief_select", {{"input", {
      {"taskId", "task_planned"}, {"briefId", "brief_fixture"}, {"optionId", "option_start"}}}});
  check(choice.ok && choice.value.at("continuation") == "start_plan" &&
            database.latest_investigation_plan("task_planned")->at("approval").at("status") ==
                "approved", "only the explicit existing start_plan choice may approve the plan");
  const auto second = start(router, "task_planned");
  const auto recorded = router.invoke("provider_list", json::object());
  check(recorded.ok && recorded.value.at("lastStart").at("planBinding") ==
          json{{"planId", "plan_fixture"}, {"planStepId", "fixture_action"},
               {"evidenceIds", json::array()}},
        "the next start must carry only the host-read saved plan binding");
  const auto before_stale = read_task(database, "task_planned");
  const auto first_terminal = *database.get_investigation_run(first);
  deliver(router, completion(first, "task_planned"));
  auto old_failure = event("agent.failed", first, "task_planned");
  old_failure["error"] = "model_timeout";
  deliver(router, old_failure);
  check(read_task(database, "task_planned") == before_stale &&
            *database.get_investigation_run(first) == first_terminal &&
            read_task(database, "task_planned").at("activeRunId") == second,
        "duplicate or late terminal events must not alter a new active run");

  // An approval wait is an active checkpoint, including when a malformed
  // completion incorrectly claims that waiting state is a final result.
  auto waiting = event("agent.waiting_approval", second, "task_planned");
  waiting["approval"] = {{"toolName", "fixture.read"}};
  deliver(router, waiting);
  check(database.get_investigation_run(second)->at("status") == "waiting_user" &&
            !database.get_investigation_run(second)->contains("endedAt") &&
            read_task(database, "task_planned").at("activeRunId") == second,
        "waiting approval must retain an active run without an end time");
  check(!router.invoke("investigation_task_start", {{"taskId", "task_planned"}}).ok,
        "an approval checkpoint must block a duplicate start");
  const auto wait_state = read_task(database, "task_planned");
  deliver(router, completion(second, "task_planned", "waiting_approval"));
  auto wrong_result_run = completion(second, "task_planned");
  wrong_result_run["result"]["runId"] = "different_run";
  deliver(router, wrong_result_run);
  auto foreign = event("agent.failed", second, "task_other");
  foreign["error"] = "model_timeout";
  deliver(router, foreign);
  deliver(router, json{{"type", "agent.completed"}, {"runId", second}, {"taskId", 42}});
  check(read_task(database, "task_planned") == wait_state &&
            database.get_investigation_run(second)->at("status") == "waiting_user",
        "non-final or mismatched notifications must leave the checkpoint intact");
  deliver(router, completion(second, "task_planned"));

  run_continuation_prompt_checks(router, database);
  run_stage_progress_checks(router, database);
  run_interrupted_plan_checks(router, database);
  run_recovery_state_checks(router, database);

  // A normal failure is recoverable only through the existing explicit host
  // recovery route; no event widens the plan or grants unattended approval.
  (void)database.create_investigation_task(task("task_retry"));
  const auto failed_id = start(router, "task_retry");
  auto failed = event("agent.failed", failed_id, "task_retry");
  failed["error"] = "model_timeout";
  deliver(router, failed);
  const auto failed_task = read_task(database, "task_retry");
  check(failed_task.at("status") == "failed" && !failed_task.contains("activeRunId") &&
            failed_task.at("lastFailure").at("code") == "timeout" &&
            failed_task.at("lastFailure").at("retryable") == true &&
            failed_task.at("lastFailure").at("detail").at("agentError") == "model_timeout" &&
            database.get_investigation_run(failed_id)->at("status") == "failed",
        "Agent failures must persist the existing recovery taxonomy");
  const auto failed_run = *database.get_investigation_run(failed_id);
  deliver(router, completion(failed_id, "task_retry"));
  check(read_task(database, "task_retry") == failed_task &&
            *database.get_investigation_run(failed_id) == failed_run,
        "a completed notification cannot overwrite a terminal failed attempt");
  check(router.invoke("investigation_task_recover", {{"input", {
      {"taskId", "task_retry"}, {"optionId", "retry"}}}}).ok,
        "the existing explicit retry option must recover a failed task");
  const auto retried = start(router, "task_retry");
  check(retried != failed_id && database.get_investigation_run(retried)->at("attempt") == 2,
        "recovery must admit one fresh bounded attempt");

  // Cancellation has its own run state and the established resume option.
  deliver(router, completion(retried, "task_retry", "cancelled"));
  const auto cancelled = read_task(database, "task_retry");
  check(cancelled.at("status") == "stopped" && !cancelled.contains("activeRunId") &&
            cancelled.at("lastFailure").at("code") == "cancelled" &&
            cancelled.at("lastFailure").at("options").at(0).at("action") == "resume" &&
            database.get_investigation_run(retried)->at("status") == "cancelled",
        "cancelled attempts must stop with an explicit resume choice");

  // An authoritative host stop/completion/failure/expiry cannot be revived by
  // the final model notification. The bound attempt may still be finalized.
  for (const auto* terminal : {"stopped", "completed", "failed", "expired"}) {
    const std::string id = std::string("task_terminal_") + terminal;
    (void)database.create_investigation_task(task(id));
    const auto active = start(router, id);
    (void)database.set_investigation_task_status(id, terminal, "2026-01-01T00:00:03Z",
                                                "2026-01-01T00:00:03Z");
    deliver(router, completion(active, id));
    const auto durable = read_task(database, id);
    check(durable.at("status") == terminal &&
              durable.at("completedAt") == "2026-01-01T00:00:03Z" &&
              !durable.contains("activeRunId"),
          "finalizing an attempt must preserve the host's terminal task decision");
  }

  // Even a non-terminal older run cannot claim a newer task binding. Exercise
  // the transaction both sequentially and while a replacement binding races it.
  for (int index = 0; index < 25; ++index) {
    const auto id = "task_race_" + std::to_string(index);
    const auto old_id = "run_old_" + std::to_string(index);
    const auto new_id = "run_new_" + std::to_string(index);
    (void)database.create_investigation_task(task(id));
    database.create_investigation_run(run(old_id, id));
    database.create_investigation_run(run(new_id, id));
    (void)database.update_investigation_task_progress(id, "investigating", "investigating",
        old_id, std::nullopt, "2026-01-01T00:00:01Z", std::nullopt);
    std::atomic<bool> ready{false};
    std::thread finish([&] {
      while (!ready.load()) std::this_thread::yield();
      (void)database.apply_investigation_run_progress(id, old_id, "completed", std::nullopt,
                                                     "2026-01-01T00:00:02Z");
    });
    std::thread replace([&] {
      while (!ready.load()) std::this_thread::yield();
      (void)database.update_investigation_task_progress(id, "investigating", "investigating",
          new_id, std::nullopt, "2026-01-01T00:00:03Z", std::nullopt);
    });
    ready.store(true);
    finish.join();
    replace.join();
    const auto fresh = read_task(database, id);
    check(fresh.at("activeRunId") == new_id && fresh.at("status") == "investigating",
          "completion/replacement races must retain the newer active run");
    deliver(router, completion(old_id, id));
    check(read_task(database, id) == fresh,
          "a stale completion cannot clear a different current active binding");
  }

  std::size_t longest_frame = 0;
  {
    std::ifstream capture(fixture_wire_path);
    std::string line;
    while (std::getline(capture, line)) {
      check(!json::parse(line, nullptr, false).is_discarded(),
            "every fixture frame must remain valid strict JSON");
      longest_frame = (std::max)(longest_frame, line.size());
    }
  }
  check(longest_frame >= 4700, "the fixture must retain a response larger than 4.7 KiB");
  check(router.invoke("agent_kill", json::object()).ok, "the local fixture must stop");
  std::cout << "Investigation lifecycle: initial/continuation prompts, bounded UTF-8 references, "
               "plan continuation, approval wait, retry, cancellation, "
               "terminal idempotency, recovery permission conflicts, conservative run interruption, "
               "stale/replayed plan guards, conflict waiting states, admissible recovery, "
               "transactional plan recheck, 25 binding races and 10 revision CAS races passed.\n";
}
}  // namespace

int main(int argc, char** argv) {
  try {
    if (argc != 2) throw std::runtime_error("usage: yukinal_investigation_lifecycle_tests <python>");
    run_tests(argv[1]);
    return 0;
  } catch (const std::exception& error) {
    std::cerr << "[investigation-lifecycle-test] " << error.what() << '\n';
    return 1;
  }
}
