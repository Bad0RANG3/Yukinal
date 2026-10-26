/**
 * 底部输入区：一句话 + 一次发送，外加两个真实的输入加速器。
 *
 * 有意保持「单卡片」的克制：高级任务配置不在主路径上。这里只负责呈现、
 * 键盘行为、输入框伸缩，以及 `@` / `/` 候选列表的交互。
 *
 * 候选列表的全部机制（触发词解析、过滤、替换、高亮、键盘导航）都留在这个
 * 模块内部，对外只暴露「有哪些服务器可提及」和一个「执行了这个命令」回调 ——
 * 这样面板不必知道弹窗的存在，而候选逻辑本身已在 composer-triggers 里被单独测试。
 *
 * 只呈现已经接入的输入能力。
 */

import {
  useCallback,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";

import {
  type AgentAudioPromptPart,
  type AgentDocumentPromptPart,
  type AgentImagePromptPart,
  type AgentPermissionMode,
  type AgentPromptPart,
  type AgentRunMode,
  type AgentReasoningEffort,
  type Environment,
} from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { usePresence } from "../../hooks/usePresence.js";
import { useAnchoredPopover } from "../../hooks/useAnchoredPopover.js";
import { composerKeyAction } from "./composer-keyboard.js";
import { ConversationSettings } from "./ConversationSettings.js";
import { ReasoningPicker } from "./ReasoningPicker.js";
import {
  audioAttachmentUrl,
  FILE_ATTACHMENT_ACCEPT,
  IMAGE_ATTACHMENT_ACCEPT,
  imageAttachmentUrl,
  readAudioAttachment,
  readDocumentAttachment,
  readImageAttachment,
  readTextFileAttachment,
} from "./image-attachments.js";
import {
  findActiveTrigger,
  matchCommands,
  matchMentions,
  replaceTrigger,
  resolveSubmission,
  type ActiveTrigger,
  type CommandContext,
  type CommandSpec,
  type MentionCandidate,
  type Submission,
} from "./composer-triggers.js";
import { approvalOptionSpec, runModeSpec } from "./run-mode.js";
import { policyEnvironmentWarning, runPolicySpec, type RunPolicyChoice } from "./run-policy.js";

/** 候选列表里的一项：命令与提及共用同一套渲染与键盘导航。 */
type Suggestion =
  | { kind: "command"; key: string; spec: CommandSpec; reason: string | null }
  | { kind: "mention"; key: string; candidate: MentionCandidate };

export function AgentComposer({
  prompt,
  onPromptChange,
  attachments,
  onAttachmentsChange,
  onSubmit,
  onStop,
  running,
  stopping,
  canSend,
  canStop,
  sendDisabledReason,
  permissionMode,
  onPermissionModeChange,
  runMode,
  onRunModeChange,
  delivery,
  onDeliveryChange,
  runPolicy,
  onRunPolicyChange,
  targetEnvironment,
  models,
  selectedModelKey,
  selectedModelLabel,
  onSelectModel,
  reasoningLevels = [],
  reasoningEffort = null,
  onReasoningEffortChange,
  mentions,
  commandContext,
  notices,
  status,
}: {
  prompt: string;
  onPromptChange: (value: string) => void;
  attachments: readonly AgentPromptPart[];
  onAttachmentsChange: (attachments: AgentPromptPart[]) => void;
  /**
   * 提交一段输入。这里已经把文本解析成「命令 / 未知命令 / 普通提问」，
   * 但**是否可用以及如何反馈由面板决定** —— 因为那取决于 sidecar、归档状态
   * 等等面板才知道的事实。关键是一条：未知或不可用的命令绝不能退化成提问。
   */
  onSubmit: (submission: Submission) => void;
  onStop: () => void;
  running: boolean;
  stopping: boolean;
  /** 当前是否允许提交（取决于 shell、sidecar、provider 与归档状态）。 */
  canSend: boolean;
  sendDisabledReason?: string;
  /** 运行中有 runId 且没有正在停止时才允许中断。 */
  canStop: boolean;
  permissionMode: AgentPermissionMode;
  onPermissionModeChange: (mode: AgentPermissionMode) => void;
  /** 这次运行允许做到什么程度；由 sidecar 权限引擎强制，不只是提示。 */
  runMode: AgentRunMode;
  onRunModeChange: (mode: AgentRunMode) => void;
  delivery: "async" | "sync";
  onDeliveryChange: (delivery: "async" | "sync") => void;
  /** 这次运行按哪套策略判定；`null` = 按环境自动（请求里不带 policyId）。 */
  runPolicy: RunPolicyChoice;
  onRunPolicyChange: (policy: RunPolicyChoice) => void;
  /** Environment this prompt will target if it is sent now. */
  targetEnvironment: Environment;
  models: ReadonlyArray<{ key: string; label: string; providerLabel?: string }>;
  selectedModelKey: string | null;
  /** 当前选中模型的完整名称。 */
  selectedModelLabel: string | null;
  onSelectModel: (key: string) => void;
  reasoningLevels?: readonly AgentReasoningEffort[];
  reasoningEffort?: AgentReasoningEffort | null;
  onReasoningEffortChange?: (value: AgentReasoningEffort | null) => void;
  /** 可被 @ 提及的服务器。 */
  mentions: readonly MentionCandidate[];
  commandContext: CommandContext;
  notices?: ReactNode;
  status?: ReactNode;
}) {
  const modeSpec = runModeSpec(runMode);
  const approvalSpec = approvalOptionSpec(permissionMode);
  const policySpec = runPolicySpec(runPolicy);
  const policyWarning = policyEnvironmentWarning(runPolicy, targetEnvironment);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [draggingAttachment, setDraggingAttachment] = useState(false);
  const [trigger, setTrigger] = useState<ActiveTrigger | null>(null);
  const [highlight, setHighlight] = useState(0);
  /** Escape 之后本次触发词不再弹窗，直到内容变化。 */
  const [dismissed, setDismissed] = useState(false);
  /** 运行设置：运行模式与批准方式都在这里。 */
  const [menuOpen, setMenuOpen] = useState(false);
  const imageAttachments = attachments.filter(
    (attachment): attachment is AgentImagePromptPart => attachment.type === "image",
  );
  const fileAttachments = attachments.filter(
    (attachment): attachment is Extract<AgentPromptPart, { type: "file" }> =>
      attachment.type === "file",
  );
  const documentAttachments = attachments.filter(
    (attachment): attachment is AgentDocumentPromptPart => attachment.type === "document",
  );
  const audioAttachments = attachments.filter(
    (attachment): attachment is AgentAudioPromptPart => attachment.type === "audio",
  );
  const settingsTriggerRef = useRef<HTMLButtonElement>(null);
  const settingsPanelRef = useRef<HTMLDivElement>(null);
  const settingsId = useId();
  const hasContent = Boolean(prompt.trim() || attachments.length);
  // Preparing a local draft does not require a connected model or runtime.
  const canEdit = !commandContext.archived;

  /**
   * 输入框随内容长高，封顶由 CSS 的 max-height 决定。
   * 必须先把 height 归零再量 scrollHeight —— 否则删字时量到的还是旧高度，框不会缩回去。
   */
  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${input.scrollHeight}px`;
  }, [prompt]);

  const suggestions: Suggestion[] = (() => {
    if (!trigger || dismissed) return [];
    if (trigger.kind === "command") {
      return matchCommands(trigger.query, commandContext).map(({ spec, reason }) => ({
        kind: "command" as const,
        key: `cmd:${spec.name}`,
        spec,
        reason,
      }));
    }
    return matchMentions(mentions, trigger.query).map((candidate) => ({
      kind: "mention" as const,
      key: `mention:${candidate.id}`,
      candidate,
    }));
  })();

  const open = suggestions.length > 0;
  const active = open ? Math.min(highlight, suggestions.length - 1) : 0;

  /* 这个输入区里有四个元素是「出现/消失」而不是「一直都在」的：候选列表、
     运行设置面板、发送按钮、运行条。它们各自都值得一段动画 —— 但 React 在条件
     变假的那一刻就把节点摘掉了，CSS 没有机会播退场。presence 让它们多留
     160ms，只影响渲染，不影响任何逻辑状态（open / menuOpen / running 仍然是
     唯一的事实来源）。 */
  const suggestionsPresence = usePresence(open, { exitAnimation: "popover-exit" });
  const settingsPresence = usePresence(menuOpen, { exitAnimation: "popover-exit" });
  useAnchoredPopover(settingsPresence.mounted, settingsTriggerRef, settingsPanelRef, 340);
  const sendPresence = usePresence(hasContent && !running, { exitAnimation: "pop-exit" });
  const runningPresence = usePresence(running, { exitAnimation: "pop-exit" });
  const showSend = hasContent && !running;
  const closeSettings = useCallback(() => {
    setMenuOpen(false);
    settingsTriggerRef.current?.focus({ preventScroll: true });
  }, []);

  /* 面板打开时把焦点送进去。
     这不是锦上添花，是这一处原先真的走不通：面板在 DOM 里排在工具栏**之前**，
     所以焦点停在「+」上往前 Tab 只会经过模型选择、发送按钮，一路离开输入区，
     永远走不到面板里 —— 键盘用户除了用鼠标，没有任何办法操作运行模式（实测）。

     依赖是 `settingsPresence.mounted` 而不是 `menuOpen`：usePresence 是在
     useEffect 里才 dispatch 的，所以 `mounted` 比 `menuOpen` 晚一帧才变真。
     盯 `menuOpen` 的话，effect 跑的那一刻面板还没进 DOM，ref 是 null，
     这一句就静默地什么也没做 —— 而且 menuOpen 之后不再变化，不会再有机会。
     用 rAF 而不是直接 focus：面板刚挂上时入场动画还没跑完，量不到位置。 */
  useLayoutEffect(() => {
    if (!menuOpen || !settingsPresence.mounted) return;
    const frame = requestAnimationFrame(() => {
      const first = settingsPanelRef.current?.querySelector<HTMLElement>("select:not([disabled])")
        ?? settingsPanelRef.current?.querySelector<HTMLElement>("button:not([disabled])");
      (first ?? settingsPanelRef.current)?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [menuOpen, settingsPresence.mounted]);

  /* Escape 关掉面板，而不是顺手把整个 Agent 面板收起来。
     useAgentPanelShell 挂了一条 window 级的 Escape（收起整个面板），
     不拦冒泡的话用户按 Esc 想关设置，结果是整块 Agent 面板没了。
     同时把焦点还给触发按钮，否则焦点随面板卸载掉到 body，键盘用户失去位置。

     挂在 <footer> 上而不是面板上：只按 Esc 时焦点可能还在触发按钮上，
     事件根本不经过面板，挂在面板上的处理函数不会触发。 */
  const handleComposerKeyDown = (event: KeyboardEvent<HTMLElement>): void => {
    if (event.key !== "Escape" || !menuOpen) return;
    event.preventDefault();
    event.stopPropagation();
    closeSettings();
  };

  /* 点到面板和「+」之外就收起。
     原先没有这一条：面板只能靠再点一次「+」关掉 —— 点别处（输入框、清单一角、
     终端）它都会一直浮在那儿，盖住下面的内容，而且没有任何提示说明该怎么关。
     模型菜单一直有这个行为，两处弹层在这件事上本该一致。

     这里要传两个区域而不是一个：触发器与面板是兄弟节点，不像模型菜单那样被同一个
     根节点包住。`pointerdown` 而非 `click` 的理由见该 hook。 */
  useDismissOnOutsidePointer(menuOpen, [settingsPanelRef, settingsTriggerRef], useCallback(() => setMenuOpen(false), []));

  /* 退场期间必须渲染**上一次的内容**。候选列表是唯一一处「内容也跟着 open
     一起清空」的弹层：`trigger` 一变 null，suggestions 立刻是空数组。如果照它
     渲染，退场动画会先把列表抽成 0 高度、再淡出一个空盒子 —— 看起来像弹层
     被拍扁了，而不是「收回去」。所以这里缓存最后一次非空的列表与高亮位置。
     这是一份渲染缓存，不是状态：它不参与任何判断，只在 closing 时被读取。 */
  const lastSuggestions = useRef<{ items: Suggestion[]; active: number }>({ items: [], active: 0 });
  const suggestionsId = useId();
  if (open) lastSuggestions.current = { items: suggestions, active };
  const visibleSuggestions = !open ? lastSuggestions.current.items : suggestions;
  const visibleActive = !open ? lastSuggestions.current.active : active;

  const syncTrigger = (value: string, caret: number | null): void => {
    const next = findActiveTrigger(value, caret ?? value.length);
    // 同一触发词内继续打字不算「重新打开」，Escape 的状态要保留。
    if (next?.start !== trigger?.start || next?.kind !== trigger?.kind) setDismissed(false);
    setTrigger(next);
    if (next?.start !== trigger?.start) setHighlight(0);
  };

  const applySuggestion = (suggestion: Suggestion): void => {
    if (!trigger) return;
    const insertion = suggestion.kind === "command" ? `/${suggestion.spec.name}` : `@${suggestion.candidate.label}`;
    const next = replaceTrigger(prompt, trigger, insertion);
    onPromptChange(next.text);
    setTrigger(null);
    setDismissed(false);
    setHighlight(0);
    // 插入后把光标放到内容末尾，用户可以接着打字。
    requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(next.caret, next.caret);
    });
  };

  const submit = (): void => {
    if (!hasContent || !canEdit) return;
    setTrigger(null);
    setDismissed(false);
    setAttachmentError(null);
    onSubmit(resolveSubmission(prompt));
  };

  const addFiles = async (files: readonly File[]): Promise<void> => {
    if (!canEdit || files.length === 0) return;
    let next: AgentPromptPart[] = [...attachments];
    let error: string | null = null;
    for (const file of files) {
      try {
        const looksLikeImage =
          file.type.startsWith("image/") || /\.(png|jpe?g|webp|gif)$/i.test(file.name);
        const looksLikePdf = file.type === "application/pdf" || /\.pdf$/i.test(file.name);
        const looksLikeAudio =
          file.type.startsWith("audio/") || /\.(wav|mp3|ogg|flac)$/i.test(file.name);
        next = [
          ...next,
          looksLikeImage
            ? await readImageAttachment(
                file,
                next.filter(
                  (attachment): attachment is AgentImagePromptPart =>
                    attachment.type === "image",
                ),
              )
            : looksLikeAudio
              ? await readAudioAttachment(file, next)
              : looksLikePdf
                ? await readDocumentAttachment(file, next)
                : await readTextFileAttachment(file, next),
        ];
      } catch (cause) {
        error = cause instanceof Error ? cause.message : String(cause);
      }
    }
    if (next.length !== attachments.length) onAttachmentsChange(next);
    setAttachmentError(error);
  };

  const removeAttachment = (attachment: AgentPromptPart): void => {
    onAttachmentsChange(attachments.filter((item) => item !== attachment));
    setAttachmentError(null);
  };

  const handlePaste = (event: ClipboardEvent<HTMLTextAreaElement>): void => {
    const files = [...event.clipboardData.items]
      .filter((item) => item.kind === "file")
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (!files.length) return;
    event.preventDefault();
    void addFiles(files);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    const suggestion = suggestions[active];
    const action = composerKeyAction({
      key: event.key, shiftKey: event.shiftKey, ctrlKey: event.ctrlKey,
      metaKey: event.metaKey, altKey: event.altKey,
      isComposing: event.nativeEvent.isComposing, keyCode: event.keyCode,
    }, { open, canComplete: Boolean(suggestion && (suggestion.kind === "mention" || suggestion.reason === null)) });
    if (action === "native") return;
    event.preventDefault();
    switch (action) {
      case "next": setHighlight((current) => (current + 1) % suggestions.length); break;
      case "previous": setHighlight((current) => (current - 1 + suggestions.length) % suggestions.length); break;
      case "dismiss": event.stopPropagation(); setDismissed(true); break;
      case "complete": if (suggestion) applySuggestion(suggestion); break;
      case "submit": submit(); break;
    }
  };

  return (
    <footer className="agent-composer" onKeyDown={handleComposerKeyDown} onBlurCapture={(event) => {
      if (!menuOpen) return;
      const next = event.relatedTarget as Node | null;
      if (!settingsPanelRef.current?.contains(next) && !settingsTriggerRef.current?.contains(next)) setMenuOpen(false);
    }}>
      <div
        className={`agent-composer-shell${draggingAttachment ? " is-dragging-attachment" : ""}`}
        onDragEnter={(event) => {
          if (!canEdit || !event.dataTransfer.types.includes("Files")) return;
          event.preventDefault();
          setDraggingAttachment(true);
        }}
        onDragOver={(event) => {
          if (!canEdit || !event.dataTransfer.types.includes("Files")) return;
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
        }}
        onDragLeave={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
            setDraggingAttachment(false);
          }
        }}
        onDrop={(event) => {
          if (!event.dataTransfer.types.includes("Files")) return;
          event.preventDefault();
          setDraggingAttachment(false);
          void addFiles([...event.dataTransfer.files]);
        }}
      >
        {draggingAttachment ? <div className="agent-composer-drop-target" aria-hidden="true"><Icon name="plus" size="md" /><span>添加到对话</span></div> : null}
        {attachments.length ? <div className="agent-composer-attachment-rail">
          {imageAttachments.length > 0 ? (
            <ul className="agent-composer-attachments" aria-label="待发送图片">
              {imageAttachments.map((image, index) => (
                <li key={`${image.name ?? "image"}-${index}`} className="agent-composer-attachment agent-composer-image-attachment">
                  <img
                    src={imageAttachmentUrl(image)}
                    alt={image.name ?? `图片 ${index + 1}`}
                    loading="lazy"
                    referrerPolicy="no-referrer"
                  />
                  <span title={image.name}>{image.name ?? `图片 ${index + 1}`}</span>
                  <button
                    type="button"
                    aria-label={`移除${image.name ?? `图片 ${index + 1}`}`}
                    title="移除图片"
                    disabled={running}
                    onClick={() => removeAttachment(image)}
                  >
                    <Icon name="close" size="xs" />
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
          {fileAttachments.length > 0 ? (
            <ul className="agent-composer-file-attachments" aria-label="待发送文本文件">
              {fileAttachments.map((attachment, index) => (
                  <li
                    key={`${attachment.name}-${index}`}
                    className="agent-composer-file-attachment"
                  >
                    <Icon name="file" size="sm" />
                    <span title={attachment.name}>{attachment.name}</span>
                    <button
                      type="button"
                      aria-label={`移除${attachment.name}`}
                      title="移除文本文件"
                      disabled={running}
                      onClick={() => removeAttachment(attachment)}
                    >
                      <Icon name="close" size="xs" />
                    </button>
                  </li>
                ))}
            </ul>
          ) : null}
          {documentAttachments.length > 0 ? (
            <ul className="agent-composer-file-attachments" aria-label="待发送 PDF">
              {documentAttachments.map((document, index) => (
                <li
                  key={`${document.name}-${index}`}
                  className="agent-composer-file-attachment"
                >
                  <Icon name="file" size="sm" />
                  <span title={document.name}>{document.name}</span>
                  <button
                    type="button"
                    aria-label={`移除${document.name}`}
                    title="移除 PDF"
                    disabled={running}
                    onClick={() => removeAttachment(document)}
                  >
                    <Icon name="close" size="xs" />
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
          {audioAttachments.length > 0 ? (
            <ul className="agent-composer-attachments" aria-label="待发送音频">
              {audioAttachments.map((audio, index) => (
                <li
                  key={`${audio.name ?? "audio"}-${index}`}
                  className="agent-composer-attachment agent-composer-audio-attachment"
                >
                  {/* 用浏览器自带的播放器预览：它是本地 data URL，不经过任何远端请求。 */}
                  <audio controls preload="metadata" src={audioAttachmentUrl(audio)} />
                  <span title={audio.name}>{audio.name ?? `音频 ${index + 1}`}</span>
                  <button
                    type="button"
                    aria-label={`移除${audio.name ?? `音频 ${index + 1}`}`}
                    title="移除音频"
                    disabled={running}
                    onClick={() => removeAttachment(audio)}
                  >
                    <Icon name="close" size="xs" />
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
        </div> : null}
        {attachmentError ? <p className="agent-composer-attachment-error" role="status">{attachmentError}</p> : null}
        {notices ? <div className="agent-input-notices">{notices}</div> : null}
        <textarea
          ref={inputRef}
          rows={1}
          aria-label="消息输入"
          /* ARIA 1.2 combobox：`aria-autocomplete` / `aria-controls` /
             `aria-activedescendant` 只有配上 `role="combobox"` 与 `aria-expanded` 才成立。
             缺这两项时读屏不知道这个文本框带弹出列表，也不会宣告列表开合 —— 用户只能
             靠视觉发现候选，键盘导航的结果不会被念出来。 */
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={open}
          aria-haspopup="listbox"
          aria-controls={open ? suggestionsId : undefined}
          aria-activedescendant={open && suggestions[active] ? `${suggestionsId}-${active}` : undefined}
          title="Enter 换行，Ctrl+Enter 发送"
          value={prompt}
          disabled={!canEdit}
          onChange={(event) => {
            const value = event.target.value;
            onPromptChange(value);
            syncTrigger(value, event.target.selectionStart);
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          onClick={(event) => syncTrigger(prompt, event.currentTarget.selectionStart)}
          onBlur={() => setTrigger(null)}
          /* 这里原本有一句占位提示（"随心输入，/ 命令，@ 提及服务器"）。
             去掉它有两个理由：
             1. 占位文本在任何浏览器里都不是可选中内容 —— 空输入框里唯一
                可见的字却选不中，会让人以为这个框坏了。
             2. 「运行中…」那半句是冗余的：下面 .agent-running-bar 已经在显示
                「运行中」+ 状态点 + 停止按钮，状态不缺这一处表达。
             输入框没有可见标签，所以 aria-label 必须保留。 */
          className="agent-composer-input"
        />
        <input
          ref={fileInputRef}
          className="agent-composer-file-input"
          type="file"
          accept={`${IMAGE_ATTACHMENT_ACCEPT},${FILE_ATTACHMENT_ACCEPT}`}
          multiple
          tabIndex={-1}
          onChange={(event) => {
            void addFiles(Array.from(event.target.files ?? [])).then(() => inputRef.current?.focus());
            event.target.value = "";
          }}
        />


        {suggestionsPresence.mounted ? (
          <ul
            className={`composer-suggestions ${suggestionsPresence.closing ? "is-closing" : ""}`}
            role="listbox"
            id={suggestionsId}
            inert={!open}
            aria-hidden={!open}
            aria-label={trigger?.kind === "command" ? "可用命令" : "可提及的服务器"}
            onAnimationEnd={suggestionsPresence.onAnimationEnd}
          >
            {visibleSuggestions.map((suggestion, index) => {
              const disabled = !open || (suggestion.kind === "command" && suggestion.reason !== null);
              return (
                <li key={suggestion.key}>
                  <button
                    type="button"
                    role="option"
                    id={`${suggestionsId}-${index}`}
                    aria-selected={index === visibleActive}
                    className={`composer-suggestion ${index === visibleActive ? "is-active" : ""}`}
                    disabled={disabled}
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => { if (open && !disabled) applySuggestion(suggestion); }}
                  >
                    <span className="composer-suggestion-name">
                      {suggestion.kind === "command" ? `/${suggestion.spec.name}` : `@${suggestion.candidate.label}`}
                    </span>
                    <span className="composer-suggestion-note">
                      {suggestion.kind === "command" ? suggestion.reason ?? suggestion.spec.description : suggestion.candidate.detail ?? ""}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        ) : null}

        {settingsPresence.mounted ? <ConversationSettings
          id={settingsId} panelRef={settingsPanelRef} open={menuOpen} closing={!menuOpen}
          running={running} onClose={closeSettings} onAnimationEnd={settingsPresence.onAnimationEnd}
          runMode={runMode} onRunModeChange={onRunModeChange}
          permissionMode={permissionMode} onPermissionModeChange={onPermissionModeChange}
          runPolicy={runPolicy} onRunPolicyChange={onRunPolicyChange} targetEnvironment={targetEnvironment}
          delivery={delivery} onDeliveryChange={onDeliveryChange}
        /> : null}

        <div className="agent-composer-toolbar">
          <div className="agent-composer-toolbar-left">
            <button type="button" className="agent-composer-tool agent-attachment-trigger"
              aria-label="添加附件" title="添加图片、文件或音频" disabled={!canEdit}
              onClick={() => { if (canEdit) { setMenuOpen(false); setTrigger(null); fileInputRef.current?.click(); } }}>
              <Icon name="plus" size="md" />
            </button>
            {models.length ? <ReasoningPicker
              choices={models} selectedModelKey={selectedModelKey}
              selectedModelTitle={selectedModelLabel ? `当前模型：${selectedModelLabel}` : null}
              onSelectModel={onSelectModel} levels={reasoningLevels} value={reasoningEffort}
              disabled={running} onChange={(value) => onReasoningEffortChange?.(value)}
            /> : <span className="agent-model-empty">未配置模型</span>}
            <button type="button" ref={settingsTriggerRef}
              className={`agent-composer-tool agent-settings-trigger${menuOpen ? " is-open" : ""}${modeSpec.readOnly ? " is-readonly" : ""}`}
              aria-label={`对话设置：${modeSpec.label}，${approvalSpec.label}`} aria-expanded={menuOpen}
              aria-haspopup="dialog" aria-controls={settingsPresence.mounted ? settingsId : undefined}
              title={`${modeSpec.label} · ${approvalSpec.label} · ${policyWarning ?? policySpec.label}`}
              onClick={() => { setMenuOpen((current) => !current); setTrigger(null); }}>
              <Icon name="settings" size="sm" /><span className="agent-control-label">{modeSpec.label}</span>
            </button>
          </div>
          <div className="agent-composer-toolbar-right">
            <div className="agent-composer-action">
              {sendPresence.mounted ? <button type="button"
                className={`agent-send-button${!showSend ? " is-closing" : ""}`}
                aria-label="发送消息" aria-hidden={!showSend} inert={!showSend}
                title={canSend ? "发送消息（Ctrl+Enter）" : sendDisabledReason ?? "请连接模型后发送"}
                aria-keyshortcuts="Control+Enter Meta+Enter" disabled={!showSend || !canSend}
                onClick={() => { if (showSend && canSend) submit(); }} onAnimationEnd={sendPresence.onAnimationEnd}>
                <Icon name="arrowUp" size="md" />
              </button> : null}
              {runningPresence.mounted ? <button type="button"
                className={`agent-stop-button${!running ? " is-closing" : ""}`}
                aria-label="停止运行" aria-hidden={!running} inert={!running}
                title={stopping ? "停止中" : "停止运行"} disabled={!running || !canStop}
                onClick={() => { if (running && canStop) onStop(); }} onAnimationEnd={runningPresence.onAnimationEnd}>
                <Icon name="stop" size="sm" /><span>{stopping ? "停止中" : "停止"}</span>
              </button> : null}
            </div>
          </div>
        </div>

      </div>
      {status || showSend ? <div className="agent-composer-meta">
        {status ? <div className="agent-composer-status" aria-label="范围与运行状态">{status}</div> : null}
        {showSend ? <span className="agent-composer-shortcut" aria-hidden="true">Ctrl+Enter 发送</span> : null}
      </div> : null}
    </footer>
  );
}
