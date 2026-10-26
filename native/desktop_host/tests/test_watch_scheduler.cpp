#include "yukinal/desktop_host/watch_scheduler.hpp"
#include <chrono>
#include <filesystem>
#include <iostream>
#include <stdexcept>
#include <vector>

using nlohmann::json;
using yukinal::desktop_host::DesktopDatabase;
using yukinal::desktop_host::WatchScheduler;
namespace {
void check(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
struct Fixture {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
      ("yukinal-watch-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
  std::unique_ptr<DesktopDatabase> database = std::make_unique<DesktopDatabase>(path);
  std::vector<json> starts, notices, activity;
  std::vector<std::string> stops;
  bool stop_confirmed = true, start_fails = false;
  std::unique_ptr<WatchScheduler> scheduler;
  Fixture() { reset(); }
  ~Fixture() { scheduler.reset(); database.reset(); std::error_code error; std::filesystem::remove_all(path, error); }
  void reset() {
    scheduler = std::make_unique<WatchScheduler>(*database,
        [&](const json& request) -> json {
          starts.push_back(request);
          if (start_fails) throw std::runtime_error("fixture admission failure");
          return {{"started", true}, {"runId", request.at("runId")}};
        }, [&](const std::string& id) { stops.push_back(id); return stop_confirmed; },
        [&](const json& event) { notices.push_back(event); },
        [&](const json& event) { activity.push_back(event); });
  }
  void create(const std::string& name = "a", const std::string& policy = "on_change") {
    const std::string at = "2026-10-20T00:00:00Z";
    (void)database->create_investigation_task({{"id", "task_" + name}, {"objective", "Monitor test login failures"},
        {"successCriteria", json::array({"Report anomalous logins from current logs"})},
        {"scope", {{"host", "local"}, {"environment", "local"}}},
        {"guardrails", {{"forbiddenTools", json::array()}, {"forbiddenPathPrefixes", json::array()}}},
        {"mode", "readonly"}, {"permissionMode", "ask"}, {"automationLevel", "readonly"},
        {"phase", "investigating"}, {"status", "pending"},
        {"budget", {{"maxSteps", 10}, {"maxRunMs", 60000}, {"maxAttempts", 3}}},
        {"createdBy", "user"}, {"createdAt", at}, {"updatedAt", at}});
    (void)database->create_investigation_schedule({{"id", name}, {"taskId", "task_" + name},
        {"status", "active"}, {"intervalSeconds", 30}, {"cooldownSeconds", 0},
        {"dedupeWindowSeconds", 1}, {"maxConcurrentRuns", 1},
        {"budget", {{"maxSteps", 10}, {"maxRunMs", 60000}, {"maxAttempts", 3}}},
        {"notificationPolicy", policy}, {"nextRunAt", at}, {"createdAt", at}, {"updatedAt", at}});
  }
  void complete(const std::string& id, const std::string& outcome, bool observed = true) {
    if (observed) scheduler->on_event({{"type", "agent.tool_result"}, {"runId", id},
        {"toolName", "server.logs"}, {"status", "success"}}, "2026-10-20T00:00:01Z");
    scheduler->on_event({{"type", "agent.completed"}, {"runId", id}, {"result", {
        {"runId", id}, {"state", "completed"}, {"text", json{{"outcome", outcome},
        {"summary", outcome == "changed" ? "Login failure burst in fixture" : "Fixture baseline"}}.dump()}}}},
        "2026-10-20T00:00:02Z");
  }
  std::string last_id() { return starts.back().at("runId"); }
};

void lifecycle() {
  Fixture f; f.create();
  f.scheduler->tick("2026-10-20T00:00:00Z");
  check(f.starts.size() == 1 && f.starts[0]["delivery"] == "async" && f.starts[0]["resume"] == true &&
      f.starts[0]["mode"] == "readonly" && f.starts[0]["taskId"] == "task_a",
      "clock must admit an asynchronous scoped readonly inspection");
  const auto first = f.last_id();
  check(f.activity.size() == 1 && f.activity.back()["stage"] == "starting" &&
      f.activity.back()["sessionId"] == "watch:" + first && f.activity.back()["serverId"].is_null(),
      "watch activity must retain its own archived session and target");
  f.scheduler->on_event({{"type", "agent.tool_call"}, {"runId", first}, {"taskId", "task_a"},
      {"callId", "logs_1"}, {"toolName", "server.logs"}, {"input", {{"private", "must not enter activity"}}}},
      "2026-10-20T00:00:01Z");
  check(f.activity.back()["stage"] == "running_tool" && f.activity.back()["tool"]["callId"] == "logs_1" &&
      !f.activity.back().contains("input") && !f.activity.back().contains("text"),
      "watch metadata must drive real tool cues without leaking transcript or tool input");
  const auto before_foreign = f.activity.size();
  f.scheduler->on_event({{"type", "agent.started"}, {"runId", first}, {"taskId", "foreign_task"}},
      "2026-10-20T00:00:01Z");
  check(f.activity.size() == before_foreign, "foreign task activity cannot enter the watch");
  // The runtime's derived stage maps onto the watch strip's existing states; a
  // change is shown as tool work, everything else as thinking.
  f.scheduler->on_event({{"type", "agent.stage"}, {"runId", first}, {"taskId", "task_a"},
      {"stage", "change"}, {"previous", "evidence"}}, "2026-10-20T00:00:01Z");
  check(f.activity.back()["stage"] == "running_tool", "a derived change stage drives the tool cue");
  f.scheduler->on_event({{"type", "agent.stage"}, {"runId", first}, {"taskId", "task_a"},
      {"stage", "verify"}, {"previous", "change"}}, "2026-10-20T00:00:01Z");
  check(f.activity.back()["stage"] == "thinking", "verification is analysis, not tool work");
  check(f.database->get_investigation_schedule("a")->at("lastRunStatus") == "running", "UI must expose actual running state");
  f.scheduler->tick("2026-10-20T00:00:30Z");
  check(f.starts.size() == 1, "long inspections must not overlap");
  f.complete(first, "baseline");
  check(f.activity.back()["stage"] == "completed" && f.activity.back()["runId"] == first,
      "quiet baseline must still update panel status independently of notification policy");
  check(f.notices.empty(), "on_change baseline must remain quiet");
  check(f.database->get_investigation_schedule_run(first)->at("status") == "succeeded", "completion must persist");
  check(f.database->get_investigation_schedule("a")->at("lastOutcome").get<std::string>().find("baseline") != std::string::npos,
      "skipped overlapping deadline cannot suppress a real completion");
  f.scheduler->tick("2026-10-20T00:01:00Z");
  check(f.starts.size() == 2, "next deadline must resume without another user prompt");
  check(f.starts.back().at("prompt").get<std::string>().find("Fixture baseline") != std::string::npos,
      "fresh inspection must receive the previous result for comparison");
  const auto next_prompt = f.starts.back().at("prompt").get<std::string>();
  const auto reference = json::parse(next_prompt.substr(next_prompt.find('\n') + 1));
  check(reference.at("baseline").is_object() && reference.at("baseline").at("runId") == first,
      "an existing successful inspection must supply a real comparison baseline");
  f.complete(f.last_id(), "changed");
  check(f.notices.size() == 1 && f.notices[0]["outcome"] == "changed", "changed evidence must notify");
  f.complete(first, "changed");
  check(f.notices.size() == 1, "late terminal replay cannot notify or replace the current outcome");
  check(f.database->get_investigation_task("task_a")->at("task").at("status") == "pending",
      "inspection completion must not terminate the durable watch task");
  f.scheduler->tick("2026-10-20T00:01:30Z");
  f.complete(f.last_id(), "no_change");
  check(f.notices.size() == 1, "unchanged inspection must stay quiet");
  f.scheduler->tick("2026-10-20T00:02:00Z");
  f.scheduler->on_event({{"type", "agent.failed"}, {"runId", f.last_id()}, {"error", "timeout"}}, "2026-10-20T00:02:01Z");
  check(f.database->get_investigation_schedule("a")->at("lastError") == "inspection_timeout",
      "a transport timeout must retain a useful failure reason");
  f.scheduler->tick("2026-10-20T00:02:30Z");
  const auto retry_prompt = f.starts.back().at("prompt").get<std::string>();
  const auto retry_reference = json::parse(retry_prompt.substr(retry_prompt.find('\n') + 1));
  check(!retry_reference.at("baseline").is_null(), "a failed attempt must not erase the comparison baseline");
}
void pause_and_recover() {
  Fixture f; f.create(); f.scheduler->tick("2026-10-20T00:00:00Z");
  const auto id = f.last_id();
  auto schedule = *f.database->get_investigation_schedule("a");
  schedule["status"] = "paused";
  (void)f.database->update_investigation_schedule(schedule);
  f.stop_confirmed = false;
  f.scheduler->tick("2026-10-20T00:00:01Z");
  check(f.stops.size() == 1 && f.database->get_investigation_schedule_run(id)->at("status") == "running",
      "pending cancellation must retain its concurrency slot");
  f.complete(id, "changed");
  check(f.database->get_investigation_schedule_run(id)->at("status") == "interrupted" && f.notices.empty(),
      "completion racing a pause must stay interrupted and quiet");
  schedule = *f.database->get_investigation_schedule("a"); schedule["status"] = "active";
  (void)f.database->update_investigation_schedule(schedule);
  f.scheduler->tick("2026-10-20T00:00:30Z");
  const auto stale = f.last_id();
  f.reset(); f.scheduler->tick("2026-10-20T00:00:31Z");
  check(f.database->get_investigation_schedule_run(stale)->at("status") == "interrupted",
      "restart must recover durable claimed/running slots");
  f.scheduler->tick("2026-10-20T00:01:00Z");
  check(f.starts.size() == 3, "restart must admit the next deadline without replaying the stale run");
}
void failures_and_policies() {
  Fixture f; f.create("a", "failed_runs_only"); f.create("b", "silent"); f.create("c", "always");
  f.scheduler->tick("2026-10-20T00:00:00Z");
  check(f.starts.size() == 3, "independent tasks should all be admitted without waiting for results");
  for (const auto& start : f.starts) f.complete(start.at("runId"), "no_change", false);
  check(f.notices.size() == 2, "absent fresh evidence must not report healthy, respecting silence policy");
  for (const auto& notice : f.notices) check(notice["outcome"] == "insufficient_evidence", "fabricated healthy output must be rejected");
  f.scheduler->tick("2026-10-20T00:00:30Z");
  const auto timed = f.last_id();
  f.scheduler->tick("2026-10-20T00:01:30Z");
  check(!f.stops.empty() && f.database->get_investigation_schedule_run(timed)->at("status") == "interrupted",
      "inspection timeouts must stop and release the run");
  Fixture failed; failed.create(); failed.start_fails = true;
  failed.scheduler->tick("2026-10-20T00:00:00Z");
  check(failed.database->get_investigation_schedule_run(failed.last_id())->at("status") == "failed" && failed.notices.size() == 1,
      "admission failure must persist and notify instead of leaving a phantom running job");
}
void long_watch() {
  Fixture f; f.create();
  for (int turn = 0; turn < 61; ++turn) {
    const auto minute = turn / 2;
    const auto now = "2026-10-20T00:" + std::string(minute < 10 ? "0" : "") +
        std::to_string(minute) + (turn % 2 ? ":30Z" : ":00Z");
    f.scheduler->tick(now);
    check(f.starts.size() == static_cast<std::size_t>(turn + 1), "continuous watch stopped before its next deadline");
    f.complete(f.last_id(), turn == 30 ? "changed" : turn == 0 ? "baseline" : "no_change");
  }
  check(f.notices.size() == 1 && f.notices[0]["outcome"] == "changed",
      "61-cycle watch must only notify the meaningful fixture change");
}
}
int main() {
  try { lifecycle(); pause_and_recover(); failures_and_policies(); long_watch(); std::cout << "watch scheduler checks passed (including 61-cycle watch)\n"; return 0; }
  catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
