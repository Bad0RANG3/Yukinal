/** Generate one bounded, host-owned investigation/deployment playbook. */

import {
  INVESTIGATION_LIMITS,
  InvestigationPlanSchema,
  OBSERVATION_WINDOW_STATUSES,
  type InvestigationPlan,
  type InvestigationPlanStep,
  type ToolTarget,
} from "@yukinal/shared";
import { randomUUID } from "node:crypto";
import { z } from "zod";

import { ToolFailure, type Tool, type ToolContext } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const PLAYBOOK_TEMPLATES = [
  "readonly_health",
  "config_edit",
  "container_restart",
  "systemd_restart",
  "package_install",
  "deploy_sequence",
  "backup_cleanup",
] as const;

const packageManager = z.enum(["apt", "dnf"]);
const packageRef = z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9+._:-]{0,127}$/, "invalid package reference");
const packageVersion = z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9+._:~^-]{0,127}$/, "invalid package version");
const serviceRef = z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,119}\.service$/, "invalid systemd service reference");
const contentRevision = z.string().regex(/^[0-9a-f]{64}$/, "invalid content revision");
const pathTemplates = ["config_edit", "backup_cleanup"] as const;

const observationWindow = z
  .strictObject({
    durationSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
    intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationIntervalSeconds),
    allowedTools: z.array(z.string().trim().min(1).max(256)).min(1).max(INVESTIGATION_LIMITS.maxObservationTools),
    successCriteria: z
      .array(z.string().trim().min(1).max(INVESTIGATION_LIMITS.maxSuccessCriterionChars))
      .min(1)
      .max(INVESTIGATION_LIMITS.maxObservationCriteria),
  })
  .superRefine((window, context) => {
    if (window.intervalSeconds > window.durationSeconds) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["intervalSeconds"],
        message: "intervalSeconds must not exceed durationSeconds",
      });
    }
  });

const deploymentOperation = z.discriminatedUnion("operation", [
  z.strictObject({
    operation: z.literal("edit_file"),
    path: z.string().trim().min(1).max(4096).refine((value) => value.startsWith("/"), "edit_file requires an absolute path"),
  }),
  z.strictObject({
    operation: z.literal("restart_container"),
    container: z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/, "invalid Docker container reference"),
  }),
  z.strictObject({
    operation: z.literal("restart_service"),
    service: serviceRef,
  }),
  z.strictObject({
    operation: z.literal("install_package"),
    manager: packageManager,
    package: packageRef,
    version: packageVersion.optional(),
  }),
]);

const input = z
  .strictObject({
    template: z.enum(PLAYBOOK_TEMPLATES),
    path: z.string().trim().min(1).max(4096).optional(),
    container: z.string().trim().min(1).max(128).regex(/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/, "invalid Docker container reference").optional(),
    service: serviceRef.optional(),
    manager: packageManager.optional(),
    package: packageRef.optional(),
    version: packageVersion.optional(),
    backupPath: z.string().trim().min(1).max(4096).optional(),
    expectedRevision: contentRevision.optional(),
    steps: z.array(deploymentOperation).min(1).max(4).optional(),
    observationWindow: observationWindow.optional(),
  })
  .superRefine((request, context) => {
    if (pathTemplates.includes(request.template as (typeof pathTemplates)[number]) && !request.path?.startsWith("/")) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["path"], message: "the selected file playbook requires an absolute path" });
    }
    if (request.template === "backup_cleanup" && !request.backupPath?.startsWith("/")) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["backupPath"], message: "backup_cleanup requires an absolute backup path" });
    }
    if (request.template === "backup_cleanup" && !request.expectedRevision) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["expectedRevision"], message: "backup_cleanup requires the ledger revision" });
    }
    if (request.template === "container_restart" && !request.container) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["container"], message: "container_restart requires a container" });
    }
    if (request.template === "systemd_restart" && !request.service) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["service"], message: "systemd_restart requires a service" });
    }
    if (request.template === "package_install" && !request.manager) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["manager"], message: "package_install requires a package manager" });
    }
    if (request.template === "package_install" && !request.package) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["package"], message: "package_install requires a package" });
    }
    if (!pathTemplates.includes(request.template as (typeof pathTemplates)[number]) && request.path !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["path"], message: "path is only valid for config_edit" });
    }
    if (request.template !== "backup_cleanup" && request.backupPath !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["backupPath"], message: "backupPath is only valid for backup_cleanup" });
    }
    if (request.template !== "backup_cleanup" && request.expectedRevision !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["expectedRevision"], message: "expectedRevision is only valid for backup_cleanup" });
    }
    if (request.template !== "container_restart" && request.container !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["container"], message: "container is only valid for container_restart" });
    }
    if (request.template !== "systemd_restart" && request.service !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["service"], message: "service is only valid for systemd_restart" });
    }
    if (request.template !== "package_install" && request.manager !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["manager"], message: "manager is only valid for package_install" });
    }
    if (request.template !== "package_install" && request.package !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["package"], message: "package is only valid for package_install" });
    }
    if (request.template !== "package_install" && request.version !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["version"], message: "version is only valid for package_install" });
    }
    if (request.template === "deploy_sequence" && request.steps === undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["steps"], message: "deploy_sequence requires at least one bounded operation" });
    }
    if (request.template !== "deploy_sequence" && request.steps !== undefined) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["steps"], message: "steps is only valid for deploy_sequence" });
    }
  });

type PlaybookInput = z.infer<typeof input>;

function makeStep(
  ordinal: number,
  target: ToolTarget,
  step: Omit<InvestigationPlanStep, "id" | "ordinal" | "attempts" | "status" | "startedAt" | "target">,
  now: string,
): InvestigationPlanStep {
  return {
    ...step,
    id: `plan_step_${randomUUID()}`,
    ordinal,
    target,
    attempts: 0,
    status: ordinal === 0 ? "running" : "pending",
    ...(ordinal === 0 ? { startedAt: now } : {}),
  };
}

function evidenceStep(
  ordinal: number,
  target: ToolTarget,
  title: string,
  purpose: string,
  allowedTool: string,
  successCriterion: string,
  now: string,
  inputBindings?: Record<string, string>,
): InvestigationPlanStep {
  return makeStep(ordinal, target, {
    kind: "evidence",
    title,
    purpose,
    allowedTools: [allowedTool],
    ...(inputBindings ? { inputBindings } : {}),
    riskLevel: "read",
    evidenceIds: [],
    successCriteria: [successCriterion],
    requiresApproval: false,
    maxAttempts: 1,
  }, now);
}

function findingStep(
  ordinal: number,
  target: ToolTarget,
  title: string,
  purpose: string,
  now: string,
): InvestigationPlanStep {
  return makeStep(ordinal, target, {
    kind: "decision",
    title,
    purpose,
    allowedTools: ["investigation.finding"],
    riskLevel: "read",
    evidenceIds: [],
    successCriteria: ["The finding classifies the observation as fact, inference or unknown and cites evidence."],
    requiresApproval: false,
    maxAttempts: 1,
  }, now);
}

function briefStep(
  ordinal: number,
  target: ToolTarget,
  title: string,
  purpose: string,
  now: string,
): InvestigationPlanStep {
  return makeStep(ordinal, target, {
    kind: "decision",
    title,
    purpose,
    allowedTools: ["investigation.brief"],
    riskLevel: "read",
    evidenceIds: [],
    successCriteria: ["The decision brief cites persisted findings and evidence, with conservative options and uncertainty."],
    requiresApproval: false,
    maxAttempts: 1,
  }, now);
}

function verificationStep(
  ordinal: number,
  target: ToolTarget,
  title: string,
  purpose: string,
  allowedTool: string,
  successCriterion: string,
  now: string,
  inputBindings?: Record<string, string>,
): InvestigationPlanStep {
  return makeStep(ordinal, target, {
    kind: "verification",
    title,
    purpose,
    allowedTools: [allowedTool],
    ...(inputBindings ? { inputBindings } : {}),
    riskLevel: "read",
    evidenceIds: [],
    successCriteria: [successCriterion],
    verificationCriteria: [successCriterion],
    requiresApproval: false,
    maxAttempts: 1,
  }, now);
}

function actionStep(
  ordinal: number,
  target: ToolTarget,
  title: string,
  purpose: string,
  allowedTool: string,
  riskLevel: "medium" | "high",
  idempotency: "conditional" | "unsafe",
  preconditions: string[],
  preview: string,
  rollback: string,
  successCriterion: string,
  now: string,
  inputBindings?: Record<string, string>,
  requiresApproval = true,
): InvestigationPlanStep {
  return makeStep(ordinal, target, {
    kind: "action",
    title,
    purpose,
    allowedTools: [allowedTool],
    ...(inputBindings ? { inputBindings } : {}),
    idempotency,
    riskLevel,
    requiresBaseline: true,
    preconditions,
    verificationCriteria: [successCriterion],
    preview,
    rollback,
    evidenceIds: [],
    successCriteria: [successCriterion],
    requiresApproval,
    maxAttempts: 1,
  }, now);
}

function buildSteps(
  request: PlaybookInput,
  target: ToolTarget,
  now: string,
  delegateMediumActions: boolean,
): InvestigationPlanStep[] {
  switch (request.template) {
    case "readonly_health":
      return [
        evidenceStep(0, target, "Collect server health", "Read the current server health and capabilities before interpreting symptoms.", "server.info", "A fresh server snapshot is persisted as evidence.", now),
        evidenceStep(1, target, "Collect recent server logs", "Read the bounded journal or system log tail so symptoms can be compared with the health snapshot.", "server.logs", "A bounded server log sample is persisted, or the host reports that no supported log source is available.", now),
        evidenceStep(2, target, "Discover server services", "Read bounded systemd or Docker service state to identify stopped or failed components.", "server.services", "A bounded service inventory is persisted, or the host reports that no supported service manager is available.", now),
        evidenceStep(3, target, "Collect container inventory", "Read the bounded Docker inventory when the target exposes Docker.", "docker.ps", "A fresh container inventory is persisted or the host reports the capability is unavailable.", now),
        findingStep(4, target, "Classify health finding", "Link one fact, inference or unknown to the collected evidence before proposing action.", now),
        briefStep(5, target, "Summarize health findings", "Present conservative next steps, cite the finding and state what remains uncertain.", now),
        verificationStep(6, target, "Recheck server health", "Perform one final read-only health check before closing the task.", "server.info", "The final health snapshot is collected without changing target state.", now),
      ];
    case "config_edit": {
      const path = request.path as string;
      const pathDescription = `the absolute path ${path}`;
      return [
        evidenceStep(0, target, "Collect server baseline", "Read server health before considering a configuration change.", "server.info", "A pre-change server baseline is persisted as evidence.", now),
        evidenceStep(1, target, "Read configuration", `Read ${pathDescription} completely and retain its content revision.`, "filesystem.read", "The complete file and its content revision are persisted as evidence.", now, { path }),
        findingStep(2, target, "Classify configuration finding", `Use the evidence for ${pathDescription} to distinguish observed content from assumptions.`, now),
        briefStep(3, target, "Prepare configuration change", `State the exact change, impact and remaining uncertainty for ${pathDescription}.`, now),
        actionStep(
          4,
          target,
          "Create guarded configuration backup",
          `Create one host-owned sibling backup of ${pathDescription} before any edit; the host chooses the destination and never overwrites an older backup.`,
          "filesystem.backup",
          "medium",
          "conditional",
          ["filesystem.read must return a complete file and a current content revision.", "The host must return a unique backupPath for this exact file."],
          `Preview only: create one bounded host-owned backup of ${path}; no configuration edit is performed until ${delegateMediumActions ? "the selected task delegation and host permission gates allow it" : "approval"}.`,
          "If backup creation fails, stop before editing and preserve the original file unchanged.",
          "A complete sibling backup is created and its path is returned for a possible later restore.",
          now,
          { path },
          !delegateMediumActions,
        ),
        actionStep(
          5,
          target,
          "Apply guarded configuration edit",
          `Edit ${pathDescription} only after ${delegateMediumActions ? "the selected task delegation and host permission gates allow the exact old/new strings" : "the user approves the exact old/new strings"} and the revision returned by filesystem.read.`,
          "filesystem.edit",
          "medium",
          "conditional",
          ["filesystem.read must return a complete file and a current content revision.", "The requested oldString must occur exactly once."],
          `Preview only: compare-and-swap one exact occurrence in ${path}; no write is performed until ${delegateMediumActions ? "the selected task delegation and host permission gates allow it" : "approval"}.`,
          `If verification fails, re-read ${path}, preserve the changed revision and propose a separate reverse edit for approval.`,
          "The guarded edit succeeds and returns a new content revision.",
          now,
          { path },
          !delegateMediumActions,
        ),
        verificationStep(6, target, "Verify configuration", `Re-read ${pathDescription} and compare the result with the approved change.`, "filesystem.read", "The file contains the approved change and remains readable.", now, { path }),
      ];
    }
    case "container_restart": {
      const container = request.container as string;
      return [
        evidenceStep(0, target, "Inspect container baseline", `Read the normalized state of container ${container} before any action.`, "docker.inspect", "A pre-change container baseline is persisted as evidence.", now, { container }),
        evidenceStep(1, target, "Collect container logs", `Read a bounded log tail for container ${container} to support the decision.`, "docker.logs", "A bounded log sample is persisted or the host reports that logs are unavailable.", now, { container }),
        findingStep(2, target, "Classify container finding", `Use the container state and logs to distinguish observed symptoms from assumptions.`, now),
        briefStep(3, target, "Prepare container restart", `Explain why a restart may help, its impact and what remains unknown.`, now),
        actionStep(
          4,
          target,
          "Restart container",
          `Restart ${container} only after explicit approval; a restart is disruptive and cannot be undone by the tool.`,
          "docker.restart",
          "high",
          "unsafe",
          ["docker.inspect must identify the intended container.", "The decision brief must describe the expected impact."],
          `Preview only: restart Docker container ${container} with the host timeout; no action is performed until approval.`,
          `A restart has no direct undo. If verification fails, preserve logs and propose a separately approved recovery or rollback plan.`,
          `Container ${container} restarts successfully and returns a success result.`,
          now,
          { container },
        ),
        verificationStep(5, target, "Verify container health", `Inspect ${container} after the restart and compare its state with the success criteria.`, "docker.inspect", `Container ${container} is running or the host provides a clear, evidence-backed failure.`, now, { container }),
      ];
    }
    case "systemd_restart": {
      const service = request.service as string;
      return [
        evidenceStep(0, target, "Collect server baseline", "Read server health before considering a systemd service restart.", "server.info", "A pre-change server baseline is persisted as evidence.", now),
        evidenceStep(1, target, "Inspect systemd service", `Read the normalized state of ${service} before any action.`, "systemd.inspect", "A pre-change systemd service baseline is persisted as evidence.", now, { service }),
        findingStep(2, target, "Classify service finding", `Use the service state to distinguish observed symptoms from assumptions about ${service}.`, now),
        briefStep(3, target, "Prepare systemd restart", `Explain why restarting ${service} may help, its impact and what remains unknown.`, now),
        actionStep(
          4,
          target,
          "Restart systemd service",
          `Restart ${service} only after explicit approval; a restart is disruptive and cannot be undone by the tool.`,
          "systemd.restart",
          "high",
          "unsafe",
          ["systemd.inspect must identify the intended service.", "The decision brief must describe the expected service impact."],
          `Preview only: restart systemd service ${service} with the host timeout; no action is performed until approval.`,
          "A restart has no direct undo. If verification fails, preserve the service evidence and propose a separately approved recovery or rollback plan.",
          `The systemd restart returns success for ${service}.`,
          now,
          { service },
        ),
        verificationStep(5, target, "Verify systemd service", `Inspect ${service} after the restart and compare its state with the success criteria.`, "systemd.inspect", `Service ${service} is active/running or the host provides a clear, evidence-backed failure.`, now, { service }),
      ];
    }
    case "package_install": {
      const manager = request.manager as "apt" | "dnf";
      const packageName = request.package as string;
      const version = request.version;
      const bindings = { manager, package: packageName };
      const installBindings = version === undefined ? bindings : { ...bindings, version };
      const versionText = version === undefined ? "the current repository version" : `exact version ${version}`;
      return [
        evidenceStep(
          0,
          target,
          "Collect package baseline",
          "Read server health before considering a package installation.",
          "server.info",
          "A fresh server baseline is persisted as evidence.",
          now,
        ),
        evidenceStep(
          1,
          target,
          "Inspect package state",
          `Read ${manager} package ${packageName} before any installation and retain its installed version if present.`,
          "package.inspect",
          "The package state is persisted as evidence, including an explicit absent result when it is not installed.",
          now,
          bindings,
        ),
        findingStep(2, target, "Classify package finding", `Use the package evidence to distinguish its observed state from assumptions about ${packageName}.`, now),
        briefStep(3, target, "Prepare package installation", `Explain the impact of installing ${packageName} (${versionText}), repository trust assumptions and remaining uncertainty.`, now),
        actionStep(
          4,
          target,
          "Install package",
          `Install ${packageName} through the explicit ${manager} manager only after approval; no sudo, flags or free-form command are accepted.`,
          "package.install",
          "high",
          "unsafe",
          ["package.inspect must identify the requested manager and package state.", "The decision brief must describe repository and service impact."],
          `Preview only: run the bounded ${manager} install for ${packageName}${version === undefined ? "" : ` at ${version}`}; no change is performed until approval.`,
          "Package installation has no automatic rollback. If verification fails, preserve the package evidence and propose a separately approved removal or configuration recovery plan.",
          `The package install returns success for ${packageName}.`,
          now,
          installBindings,
        ),
        verificationStep(
          5,
          target,
          "Verify package installation",
          `Inspect ${packageName} after installation and compare the observed version with the approved ${versionText}.`,
          "package.inspect",
          `The package is installed${version === undefined ? "" : ` at version ${version}`} and the host returns a normalized state.`,
          now,
          bindings,
        ),
      ];
    }
    case "backup_cleanup": {
      const path = request.path as string;
      const backupPath = request.backupPath as string;
      const expectedRevision = request.expectedRevision as string;
      const availableBinding = { path, status: "available" };
      const cleanupBinding = { path, backupPath, expectedRevision };
      return [
        evidenceStep(
          0,
          target,
          "Inspect host backup ledger",
          `Read the host-owned recovery ledger for ${path} before proposing cleanup. This is metadata only and does not prove the remote sibling still exists.`,
          "filesystem.backup.list",
          "The host returns the bounded available-backup ledger for this task and target path.",
          now,
          availableBinding,
        ),
        findingStep(
          1,
          target,
          "Classify recovery copy",
          `Confirm that ${backupPath} is an observed, task-owned recovery copy with revision ${expectedRevision}; do not infer remote existence from the ledger alone.`,
          now,
        ),
        briefStep(
          2,
          target,
          "Prepare backup cleanup decision",
          `Explain the impact and irreversibility of deleting ${backupPath}, and state that a new backup would be required for future recovery.`,
          now,
        ),
        actionStep(
          3,
          target,
          "Delete host-owned recovery copy",
          `Delete ${backupPath} only after explicit approval, exact task ownership, revision binding and a remote consistency check.`,
          "filesystem.backup.cleanup",
          "medium",
          "conditional",
          [
            "filesystem.backup.list must report this path as an available ledger row for the current task.",
            "The recorded revision must remain the exact expectedRevision.",
          ],
          `Preview only: verify and delete the one host-owned sibling backup ${backupPath}; no arbitrary remote path is accepted.`,
          "Cleanup is irreversible. If future recovery is needed, create a new backup after re-reading the target file.",
          "The remote recovery copy is removed and its host ledger row is consumed.",
          now,
          cleanupBinding,
        ),
        verificationStep(
          4,
          target,
          "Verify recovery copy cleanup",
          `Re-read the available backup ledger for ${path} and confirm that ${backupPath} is no longer available.`,
          "filesystem.backup.list",
          "The cleaned backup no longer appears as an available row for this task.",
          now,
          availableBinding,
        ),
      ];
    }
    case "deploy_sequence": {
      const operations = request.steps ?? [];
      const steps: InvestigationPlanStep[] = [];
      let ordinal = 0;
      steps.push(
        evidenceStep(
          ordinal++,
          target,
          "Collect deployment baseline",
          "Read server health before considering the bounded deployment sequence.",
          "server.info",
          "A fresh server baseline is persisted as evidence.",
          now,
        ),
      );
      for (const operation of operations) {
        if (operation.operation === "edit_file") {
          steps.push(
            evidenceStep(
              ordinal++,
              target,
              "Read configuration before deployment",
              `Read ${operation.path} completely and retain its content revision before any edit.`,
              "filesystem.read",
              "The complete file and its content revision are persisted as evidence.",
              now,
              { path: operation.path },
            ),
          );
        } else if (operation.operation === "restart_container") {
          steps.push(
            evidenceStep(
              ordinal++,
              target,
              "Inspect container before deployment",
              `Read the normalized state of container ${operation.container} before any restart.`,
              "docker.inspect",
              "A pre-change container state is persisted as evidence.",
              now,
              { container: operation.container },
            ),
            evidenceStep(
              ordinal++,
              target,
              "Collect container logs before deployment",
              `Collect a bounded log tail for container ${operation.container} to support the decision.`,
              "docker.logs",
              "A bounded container log sample is persisted or the host reports that logs are unavailable.",
              now,
              { container: operation.container },
            ),
          );
        } else if (operation.operation === "restart_service") {
          steps.push(
            evidenceStep(
              ordinal++,
              target,
              "Inspect service before deployment",
              `Read the normalized state of systemd service ${operation.service} before any restart.`,
              "systemd.inspect",
              "A pre-change systemd service state is persisted as evidence.",
              now,
              { service: operation.service },
            ),
          );
        } else {
          steps.push(
            evidenceStep(
              ordinal++,
              target,
              "Inspect package before deployment",
              `Read the normalized ${operation.manager} state of package ${operation.package} before any installation.`,
              "package.inspect",
              "A pre-change package state is persisted as evidence, including an explicit absent result.",
              now,
              { manager: operation.manager, package: operation.package },
            ),
          );
        }
      }
      steps.push(
        findingStep(
          ordinal++,
          target,
          "Classify deployment finding",
          "Separate observed evidence from assumptions before proposing any deployment action.",
          now,
        ),
        briefStep(
          ordinal++,
          target,
          "Prepare deployment decision",
          "Explain the bounded sequence, impact, verification and remaining uncertainty before approval.",
          now,
        ),
      );
      for (const operation of operations) {
        if (operation.operation === "edit_file") {
          steps.push(
            actionStep(
              ordinal++,
              target,
              "Create deployment configuration backup",
              `Create one host-owned sibling backup of ${operation.path} before editing; the host chooses the destination and never overwrites an older backup.`,
              "filesystem.backup",
              "medium",
              "conditional",
              ["The pre-deployment filesystem.read must return a complete file and a current content revision.", "The host must return a unique backupPath for this exact file."],
              `Preview only: create one bounded host-owned backup of ${operation.path}; no edit is performed until ${delegateMediumActions ? "the selected task delegation and host permission gates allow it" : "approval"}.`,
              "If backup creation fails, stop this operation before editing and preserve the original file unchanged.",
              "A complete sibling backup is created and its path is returned for a possible later restore.",
              now,
              { path: operation.path },
              !delegateMediumActions,
            ),
            actionStep(
              ordinal++,
              target,
              "Apply guarded configuration edit",
              `Edit ${operation.path} only after ${delegateMediumActions ? "the selected task delegation and host permission gates allow the exact old/new strings" : "approval of the exact old/new strings"} and the captured revision.`,
              "filesystem.edit",
              "medium",
              "conditional",
              ["filesystem.read must return a complete file and a current content revision.", "The requested oldString must occur exactly once."],
              `Preview only: compare-and-swap one exact occurrence in ${operation.path}; no write is performed until ${delegateMediumActions ? "the selected task delegation and host permission gates allow it" : "approval"}.`,
              `If verification fails, preserve the changed revision and propose a separately approved reverse edit for ${operation.path}.`,
              "The guarded edit succeeds and returns a new content revision.",
              now,
              { path: operation.path },
              !delegateMediumActions,
            ),
            verificationStep(
              ordinal++,
              target,
              "Verify configuration deployment",
              `Re-read ${operation.path} and compare it with the approved change.`,
              "filesystem.read",
              "The file contains the approved change and remains readable.",
              now,
              { path: operation.path },
            ),
          );
        } else if (operation.operation === "restart_container") {
          steps.push(
            actionStep(
              ordinal++,
              target,
              "Restart deployment container",
              `Restart ${operation.container} only after explicit approval; the restart is disruptive and has no direct undo.`,
              "docker.restart",
              "high",
              "unsafe",
              ["docker.inspect must identify the intended container.", "The decision brief must describe the expected impact."],
              `Preview only: restart Docker container ${operation.container}; no action is performed until approval.`,
              `A restart has no direct undo. If verification fails, preserve logs and propose a separately approved recovery plan.`,
              `Container ${operation.container} restarts successfully and returns a success result.`,
              now,
              { container: operation.container },
            ),
            verificationStep(
              ordinal++,
              target,
              "Verify container deployment",
              `Inspect ${operation.container} after the restart and compare it with the success criteria.`,
              "docker.inspect",
              `Container ${operation.container} is running or the host provides a clear evidence-backed failure.`,
              now,
              { container: operation.container },
            ),
          );
        } else if (operation.operation === "restart_service") {
          steps.push(
            actionStep(
              ordinal++,
              target,
              "Restart deployment service",
              `Restart systemd service ${operation.service} only after explicit approval; the restart is disruptive and has no direct undo.`,
              "systemd.restart",
              "high",
              "unsafe",
              ["systemd.inspect must identify the intended .service unit.", "The decision brief must describe the expected impact."],
              `Preview only: restart systemd service ${operation.service}; no action is performed until approval.`,
              `A service restart has no direct undo. If verification fails, preserve the service state and propose a separately approved recovery plan.`,
              `Systemd service ${operation.service} restarts successfully and returns a success result.`,
              now,
              { service: operation.service },
            ),
            verificationStep(
              ordinal++,
              target,
              "Verify deployment service",
              `Inspect ${operation.service} after the restart and compare it with the success criteria.`,
              "systemd.inspect",
              `Systemd service ${operation.service} is active/running or the host provides a clear, evidence-backed failure.`,
              now,
              { service: operation.service },
            ),
          );
        } else {
          const versionText = operation.version === undefined ? "the current repository version" : `exact version ${operation.version}`;
          const installBindings: Record<string, string> = {
            manager: operation.manager,
            package: operation.package,
            ...(operation.version === undefined ? {} : { version: operation.version }),
          };
          steps.push(
            actionStep(
              ordinal++,
              target,
              "Install deployment package",
              `Install ${operation.package} through ${operation.manager} at ${versionText} only after explicit approval; no sudo or free-form flags are accepted.`,
              "package.install",
              "high",
              "unsafe",
              ["package.inspect must identify the requested manager and package state.", "The decision brief must describe repository and service impact."],
              `Preview only: run the bounded ${operation.manager} install for ${operation.package}${operation.version === undefined ? "" : ` at ${operation.version}`}; no action is performed until approval.`,
              "Package installation has no automatic rollback. If verification fails, preserve the package evidence and propose a separately approved removal or configuration recovery plan.",
              `The package install returns success for ${operation.package}.`,
              now,
              installBindings,
            ),
            verificationStep(
              ordinal++,
              target,
              "Verify deployment package",
              `Inspect ${operation.package} after installation and compare it with the approved ${versionText}.`,
              "package.inspect",
              `The package is installed${operation.version === undefined ? "" : ` at version ${operation.version}`} and the host returns a normalized state.`,
              now,
              { manager: operation.manager, package: operation.package },
            ),
          );
        }
      }
      return steps;
    }
  }
}

export function investigationPlaybookTool(host: HostRpcClient): Tool<PlaybookInput, InvestigationPlan> {
  return {
    name: "investigation.playbook",
    description:
      "Generate a bounded, host-owned playbook for a supported maintenance shape. The tool only records " +
      "an ordered dry-run plan; it never performs a remote write. Choose readonly_health, config_edit, " +
      "container_restart, systemd_restart, package_install, backup_cleanup or deploy_sequence on a resolved remote server target. backup_cleanup " +
      "requires an exact path, backupPath and ledger revision; it only deletes one host-owned backup after approval. deploy_sequence accepts " +
      "only bounded edit_file, restart_container, restart_service and install_package operations, and expands them into evidence, decision, " +
      "approved action and verification steps. Action steps include a baseline requirement, preview, " +
      "verification, rollback text and explicit approval, and still pass through the normal host plan and " +
      "permission gates. An optional observationWindow can keep sampling the final read-only verification " +
      "until a bounded deadline; its tools must be a subset of that final verification step.",
    risk: "read",
    timeoutMs: 10_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context: ToolContext) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.playbook requires a durable task", "invalid_input", false);
      }
      if (context.target.host !== "remote" || !context.target.serverId) {
        throw new ToolFailure(
          "investigation.playbook currently requires a resolved remote server target",
          "unsupported",
          false,
        );
      }
      const now = new Date().toISOString();
      const delegateMediumActions =
        context.permissionMode === "auto" &&
        context.mode === "goal" &&
        (context.target.environment === "development" || context.target.environment === "staging");
      const steps = buildSteps(request, context.target, now, delegateMediumActions);
      if (request.observationWindow) {
        const finalStep = steps.at(-1);
        if (!finalStep || finalStep.kind !== "verification") {
          throw new ToolFailure(
            "observationWindow requires a final verification step",
            "invalid_input",
            false,
          );
        }
        const unsupportedTools = request.observationWindow.allowedTools.filter(
          (tool) => !finalStep.allowedTools.includes(tool),
        );
        if (unsupportedTools.length > 0) {
          throw new ToolFailure(
            "observationWindow.allowedTools must be a subset of the final verification tools",
            "invalid_input",
            false,
            { unsupportedTools, verificationTools: finalStep.allowedTools },
          );
        }
      }
      const plan: InvestigationPlan = {
        id: `plan_${randomUUID()}`,
        taskId: context.taskId,
        revision: 1,
        status: "active",
        createdAt: now,
        updatedAt: now,
        currentStepId: steps[0]?.id,
        ...(request.observationWindow
          ? {
              observationWindow: {
                ...request.observationWindow,
                status: OBSERVATION_WINDOW_STATUSES[0],
                sampleCount: 0,
              },
            }
          : {}),
        steps,
      };
      const response = await host.recordPlan({ plan }, context.signal);
      if (response.recorded) return InvestigationPlanSchema.parse(response.plan);
      throw new ToolFailure(response.error.message, response.error.code, response.error.retryable, response.error.detail);
    },
  };
}

export const investigationPlaybookInputSchema = input;
