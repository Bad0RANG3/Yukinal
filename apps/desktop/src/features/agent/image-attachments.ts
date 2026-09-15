import {
  AGENT_PROMPT_LIMITS,
  type AgentAudioMediaType,
  type AgentAudioPromptPart,
  type AgentDocumentPromptPart,
  type AgentImageMediaType,
  type AgentImagePromptPart,
  type AgentPromptPart,
  type AgentTextFilePromptPart,
} from "@yukinal/shared";

export const IMAGE_ATTACHMENT_ACCEPT = "image/png,image/jpeg,image/webp,image/gif";
export const TEXT_FILE_ATTACHMENT_ACCEPT =
  "text/*,.txt,.md,.markdown,.json,.jsonl,.csv,.tsv,.xml,.yaml,.yml,.toml,.log,.ini,.conf,.config,.sh,.bash,.zsh,.fish,.ps1,.js,.jsx,.ts,.tsx,.py,.rb,.go,.rs,.java,.kt,.c,.h,.cpp,.hpp,.css,.html,.sql";
export const PDF_ATTACHMENT_ACCEPT = "application/pdf,.pdf";
/**
 * Audio, identified by magic bytes on the way in like every other attachment. The browser's
 * MIME list and the extensions are only here to make the file picker show the files: nothing
 * trusts either of them.
 */
export const AUDIO_ATTACHMENT_ACCEPT =
  "audio/wav,audio/x-wav,audio/wave,audio/mpeg,audio/mp3,audio/ogg,audio/flac,.wav,.mp3,.ogg,.flac";
export const FILE_ATTACHMENT_ACCEPT = `${TEXT_FILE_ATTACHMENT_ACCEPT},${PDF_ATTACHMENT_ACCEPT},${AUDIO_ATTACHMENT_ACCEPT}`;

export async function readImageAttachment(
  file: File,
  existing: readonly AgentImagePromptPart[],
): Promise<AgentImagePromptPart> {
  if (existing.length >= AGENT_PROMPT_LIMITS.maxImages) {
    throw new Error(`每条消息最多添加 ${AGENT_PROMPT_LIMITS.maxImages} 张图片。`);
  }
  if (file.size <= 0) throw new Error("不能添加空文件。");
  if (file.size > AGENT_PROMPT_LIMITS.maxImageBytes) {
    throw new Error(`每张图片不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxImageBytes)}。`);
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const mediaType = detectImageMediaType(bytes);
  if (!mediaType) throw new Error("文件内容不是受支持的 PNG、JPEG、WebP 或 GIF 图片。");

  if (
    inlineAttachmentBytes(existing) + bytes.byteLength >
    AGENT_PROMPT_LIMITS.maxTotalInlineBytes
  ) {
    throw new Error(
      `本条消息的图片与 PDF 总大小不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxTotalInlineBytes)}。`,
    );
  }

  const name = sanitizeImageName(file.name);
  return {
    type: "image",
    mediaType,
    data: bytesToBase64(bytes),
    ...(name ? { name } : {}),
  };
}

export async function readAudioAttachment(
  file: File,
  existing: readonly AgentPromptPart[],
): Promise<AgentAudioPromptPart> {
  const audios = existing.filter(
    (part): part is AgentAudioPromptPart => part.type === "audio",
  );
  if (audios.length >= AGENT_PROMPT_LIMITS.maxAudios) {
    throw new Error(`每条消息最多添加 ${AGENT_PROMPT_LIMITS.maxAudios} 段音频。`);
  }
  if (file.size <= 0) throw new Error("不能添加空文件。");
  if (file.size > AGENT_PROMPT_LIMITS.maxAudioBytes) {
    throw new Error(`每段音频不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxAudioBytes)}。`);
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const mediaType = detectAudioMediaType(bytes);
  if (!mediaType) {
    throw new Error("文件内容不是受支持的 WAV、MP3、OGG 或 FLAC 音频。");
  }
  // 与图片、PDF 共用同一个总预算：受约束的是那一帧，而不是某一种附件。
  if (
    inlineAttachmentBytes(existing) + bytes.byteLength >
    AGENT_PROMPT_LIMITS.maxTotalInlineBytes
  ) {
    throw new Error(
      `本条消息的图片、PDF 与音频总大小不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxTotalInlineBytes)}。`,
    );
  }
  const name = sanitizeAudioName(file.name);
  return {
    type: "audio",
    mediaType,
    data: bytesToBase64(bytes),
    ...(name ? { name } : {}),
  };
}

export async function readDocumentAttachment(
  file: File,
  existing: readonly AgentPromptPart[],
): Promise<AgentDocumentPromptPart> {
  const documents = existing.filter(
    (part): part is AgentDocumentPromptPart => part.type === "document",
  );
  if (documents.length >= AGENT_PROMPT_LIMITS.maxDocuments) {
    throw new Error(`每条消息最多添加 ${AGENT_PROMPT_LIMITS.maxDocuments} 个 PDF。`);
  }
  if (file.size <= 0) throw new Error("不能添加空文件。");
  if (file.size > AGENT_PROMPT_LIMITS.maxDocumentBytes) {
    throw new Error(`每个 PDF 不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxDocumentBytes)}。`);
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (!looksLikePdf(bytes)) {
    throw new Error("文件内容不是有效的 PDF。");
  }
  if (
    inlineAttachmentBytes(existing) + bytes.byteLength >
    AGENT_PROMPT_LIMITS.maxTotalInlineBytes
  ) {
    throw new Error(
      `本条消息的图片与 PDF 总大小不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxTotalInlineBytes)}。`,
    );
  }
  const name = sanitizeDocumentName(file.name);
  if (!name) throw new Error("PDF 需要一个可见文件名。");
  return {
    type: "document",
    mediaType: "application/pdf",
    data: bytesToBase64(bytes),
    name,
  };
}

export async function readTextFileAttachment(
  file: File,
  existing: readonly AgentPromptPart[],
): Promise<AgentTextFilePromptPart> {
  const files = existing.filter(
    (part): part is AgentTextFilePromptPart => part.type === "file",
  );
  if (files.length >= AGENT_PROMPT_LIMITS.maxFiles) {
    throw new Error(`每条消息最多添加 ${AGENT_PROMPT_LIMITS.maxFiles} 个文本文件。`);
  }
  if (file.size <= 0) throw new Error("不能添加空文件。");
  if (file.size > AGENT_PROMPT_LIMITS.maxFileBytes) {
    throw new Error(`每个文本文件不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxFileBytes)}。`);
  }

  const bytes = new Uint8Array(await file.arrayBuffer());
  let data: string;
  try {
    data = new TextDecoder("utf-8", { fatal: true }).decode(bytes).replace(/^\uFEFF/, "");
  } catch {
    throw new Error("文本文件必须是有效的 UTF-8 内容。");
  }
  if (
    data.length === 0 ||
    /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(data)
  ) {
    throw new Error("文本文件不能包含二进制控制字符。");
  }

  const existingBytes = files.reduce(
    (sum, attachment) => sum + new TextEncoder().encode(attachment.data).byteLength,
    0,
  );
  if (existingBytes + bytes.byteLength > AGENT_PROMPT_LIMITS.maxTotalFileBytes) {
    throw new Error(
      `本条消息的文本文件总大小不能超过 ${formatBytes(AGENT_PROMPT_LIMITS.maxTotalFileBytes)}。`,
    );
  }

  const name = sanitizeFileName(file.name);
  if (!name) throw new Error("文本文件需要一个可见文件名。");
  return {
    type: "file",
    mediaType: "text/plain",
    data,
    name,
  };
}

export function promptPartsWithAttachments(
  text: string,
  attachments: readonly AgentPromptPart[],
): AgentPromptPart[] {
  const trimmed = text.trim();
  return [
    ...(trimmed ? [{ type: "text" as const, text: trimmed }] : []),
    ...attachments,
  ];
}

export function imageAttachmentUrl(image: AgentImagePromptPart): string {
  return `data:${image.mediaType};base64,${image.data}`;
}

/** 预览用：把内联字节交给 `<audio>`，与图片一样不经过任何远端 URL。 */
export function audioAttachmentUrl(audio: AgentAudioPromptPart): string {
  return `data:${audio.mediaType};base64,${audio.data}`;
}

/**
 * 音频格式按**魔数**判定，不看扩展名也不看浏览器的 MIME 猜测。
 *
 * 四种都收，因为不同 Provider 能带的形式不同（OpenAI 只认 WAV/MP3，Gemini 还能带 OGG/FLAC）：
 * 附件层收下的东西，由 adapter 决定它能不能发，并且必须**明确报错**而不是丢掉。
 */
function detectAudioMediaType(bytes: Uint8Array): AgentAudioMediaType | null {
  // RIFF....WAVE
  if (
    bytes.length >= 12 &&
    bytes[0] === 0x52 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x46 &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x41 &&
    bytes[10] === 0x56 &&
    bytes[11] === 0x45
  ) {
    return "audio/wav";
  }
  // ID3 标签头，或一个 MPEG 音频帧同步（11 个 1）。
  if (
    (bytes.length >= 3 && bytes[0] === 0x49 && bytes[1] === 0x44 && bytes[2] === 0x33) ||
    (bytes.length >= 2 && bytes[0] === 0xff && (bytes[1]! & 0xe0) === 0xe0)
  ) {
    return "audio/mpeg";
  }
  // OggS
  if (
    bytes.length >= 4 &&
    bytes[0] === 0x4f &&
    bytes[1] === 0x67 &&
    bytes[2] === 0x67 &&
    bytes[3] === 0x53
  ) {
    return "audio/ogg";
  }
  // fLaC
  if (
    bytes.length >= 4 &&
    bytes[0] === 0x66 &&
    bytes[1] === 0x4c &&
    bytes[2] === 0x61 &&
    bytes[3] === 0x43
  ) {
    return "audio/flac";
  }
  return null;
}

function detectImageMediaType(bytes: Uint8Array): AgentImageMediaType | null {
  if (
    bytes.length >= 8 &&
    bytes[0] === 0x89 &&
    bytes[1] === 0x50 &&
    bytes[2] === 0x4e &&
    bytes[3] === 0x47 &&
    bytes[4] === 0x0d &&
    bytes[5] === 0x0a &&
    bytes[6] === 0x1a &&
    bytes[7] === 0x0a
  ) {
    return "image/png";
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    bytes.length >= 6 &&
    bytes[0] === 0x47 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x38 &&
    (bytes[4] === 0x37 || bytes[4] === 0x39) &&
    bytes[5] === 0x61
  ) {
    return "image/gif";
  }
  if (
    bytes.length >= 12 &&
    bytes[0] === 0x52 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x46 &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp";
  }
  return null;
}

function looksLikePdf(bytes: Uint8Array): boolean {
  return bytes
    .subarray(0, Math.min(bytes.length, 1024))
    .some(
      (_, index, header) =>
        index + 5 <= header.length &&
        header[index] === 0x25 &&
        header[index + 1] === 0x50 &&
        header[index + 2] === 0x44 &&
        header[index + 3] === 0x46 &&
        header[index + 4] === 0x2d,
    );
}

function bytesToBase64(bytes: Uint8Array): string {
  const chunkSize = 0x8000;
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

function decodedBase64Bytes(value: string): number {
  const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
  return (value.length / 4) * 3 - padding;
}

function inlineAttachmentBytes(attachments: readonly AgentPromptPart[]): number {
  return attachments.reduce(
    (sum, attachment) =>
      attachment.type === "image" ||
      attachment.type === "document" ||
      attachment.type === "audio"
        ? sum + decodedBase64Bytes(attachment.data)
        : sum,
    0,
  );
}

function sanitizeImageName(value: string): string | undefined {
  const name = value.split(/[\\/]/).at(-1)?.trim();
  return name ? name.slice(0, AGENT_PROMPT_LIMITS.maxImageNameChars) : undefined;
}

function sanitizeFileName(value: string): string | undefined {
  const name = value.split(/[\\/]/).at(-1)?.trim();
  return name ? name.slice(0, AGENT_PROMPT_LIMITS.maxFileNameChars) : undefined;
}

function sanitizeDocumentName(value: string): string | undefined {
  const name = value.split(/[\\/]/).at(-1)?.trim();
  return name ? name.slice(0, AGENT_PROMPT_LIMITS.maxDocumentNameChars) : undefined;
}

function sanitizeAudioName(value: string): string | undefined {
  const name = value.split(/[\\/]/).at(-1)?.trim();
  return name ? name.slice(0, AGENT_PROMPT_LIMITS.maxAudioNameChars) : undefined;
}

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))} MiB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KiB`;
  return `${bytes} B`;
}
