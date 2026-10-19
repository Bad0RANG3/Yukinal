/**
 * 回复落库失败时的可见提示。
 *
 * `recordAssistantMessage` 返回 false 表示这次回复没有写进任何会话：
 * 运行期间用户可能正好删掉了当前对话。以前这件事被静默吞掉，
 * 界面看起来一切正常，下次打开历史却找不到这条回答。
 *
 * 这里只把状态翻译成一句话，不碰 UI，因此可以直接单测。
 */
export const ASSISTANT_REPLY_NOT_SAVED =
  "回复未能保存到对话记录（当前对话可能已被删除）；内容仍在当前动态里。";

/** 只有明确的 false 是失败；undefined 表示调用方不关心落库（预览等）。 */
export function assistantReplyNotSaved(persisted: boolean | void): string | null {
  return persisted === false ? ASSISTANT_REPLY_NOT_SAVED : null;
}
