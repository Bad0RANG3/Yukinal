/**
 * Agent 回复的 Markdown 渲染。
 *
 * 输出文本从来不是「模型写了什么就显示什么」：模型几乎总是带结构回来（标题、列表、
 * 表格、代码块），原样铺成一行只会让人读不下去。这里把 `lib/markdown.js` 解析出来的
 * 结构映射成元素。
 *
 * 三条硬规则：
 *
 * 1. **没有 `dangerouslySetInnerHTML`。** 正文是不可信文本，它只会变成我们自己创建的
 *    元素里的文字节点。解析器不认 HTML，这里也不把它当 HTML。
 * 2. **链接只在外部浏览器打开，`#fragment` 例外。** 外部链接走 opener 能力允许的
 *    `http(s)` / `mailto` URL，不让 WebView 导航离开应用；页内锚点则跳到本组件渲染
 *    的标题（标题带稳定 id）。未知 scheme 在解析阶段已经是普通文字。
 * 3. **远程图片需要明确同意。** 默认只显示替代文字和加载按钮；用户点击后才会为
 *    同一个 `http(s)` URL 创建带 `no-referrer` 的 `<img>`。
 *
 * 关键词着色（`KeywordText`）在正文里全量保留 —— Markdown 只负责结构，哪些词是
 * 错误、路径还是标识符，与原来一样由 token 规则决定。
 */

import { memo, useState, type ReactNode } from "react";

import { errorMessage } from "../lib/format.js";
import { openExternalUrl } from "../lib/external.js";
import { parseMarkdown, type Block, type ColumnAlign, type Inline } from "../lib/markdown.js";
import { KeywordText } from "./KeywordText.js";

const HEADING_TAG = { 1: "h1", 2: "h2", 3: "h3", 4: "h4", 5: "h5", 6: "h6" } as const;

type HeadingIds = Map<Block, string>;

export const MarkdownText = memo(function MarkdownText({ text, className }: { text: string; className?: string }) {
  const blocks = parseMarkdown(text);
  const headingIds = assignHeadingIds(blocks);
  return (
    <div className={className} data-markdown-root="">
      {blocks.map((block, index) => (
        <BlockView block={block} headingIds={headingIds} key={index} />
      ))}
    </div>
  );
});

/**
 * Give every heading a stable, deduplicated id so `[text](#anchor)` can jump to it.
 *
 * Markdown headings have no ids of their own, and the renderer deliberately does not
 * navigate the WebView. Without this, an in-page link reached the external opener and
 * threw (`#…` is not an allowed opener scheme) — a link that looked clickable and did
 * nothing visible.
 */
export function headingSlug(text: string, taken: ReadonlySet<string> = new Set()): string {
  const base =
    text
      .trim()
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s_-]/gu, "")
      .replace(/\s+/gu, "-")
      .replace(/-+/gu, "-")
      .replace(/^-|-$/gu, "") || "section";
  let slug = base;
  let suffix = 2;
  while (taken.has(slug)) slug = base + "-" + suffix++;
  return slug;
}

function assignHeadingIds(blocks: Block[]): HeadingIds {
  const ids: HeadingIds = new Map();
  const taken = new Set<string>();
  const visit = (children: Block[]): void => {
    for (const block of children) {
      if (block.kind === "heading") {
        const slug = headingSlug(inlineText(block.content), taken);
        taken.add(slug);
        ids.set(block, slug);
      } else if (block.kind === "quote") {
        visit(block.blocks);
      } else if (block.kind === "list") {
        for (const item of block.items) visit(item.blocks);
      }
    }
  };
  visit(blocks);
  return ids;
}

function inlineText(nodes: Inline[]): string {
  return nodes
    .map((node) => {
      switch (node.kind) {
        case "text":
        case "code":
          return node.text;
        case "strong":
        case "emphasis":
        case "strike":
        case "link":
          return inlineText(node.children);
        case "image":
          return node.alt;
        case "footnote":
          return "[" + node.label + "]";
        case "break":
          return " ";
      }
    })
    .join("");
}

/** `#…` is an in-page jump; everything else still goes through the restricted opener. */
export function markdownLinkTarget(href: string): { kind: "anchor"; id: string } | { kind: "external"; url: string } {
  if (!href.startsWith("#")) return { kind: "external", url: href };
  return { kind: "anchor", id: decodeAnchorId(href) };
}

export function decodeAnchorId(href: string): string {
  try {
    return decodeURIComponent(href.slice(1));
  } catch {
    return href.slice(1);
  }
}

/**
 * Open an external link and turn a rejection into a visible sentence.
 *
 * Exported with an injectable opener so the failure path is testable without a desktop
 * host: an unhandled rejection would leave the click silently doing nothing.
 */
export async function openMarkdownExternalLink(
  href: string,
  open: (url: string) => Promise<unknown> = openExternalUrl,
): Promise<string | null> {
  try {
    await open(href);
    return null;
  } catch (cause) {
    return "无法打开链接：" + errorMessage(cause);
  }
}

function BlockView({ block, headingIds }: { block: Block; headingIds: HeadingIds }) {
  switch (block.kind) {
    case "heading": {
      const Tag = HEADING_TAG[clampLevel(block.level)];
      const id = headingIds.get(block);
      return (
        <Tag className="md-heading" id={id}>
          <InlineView nodes={block.content} />
        </Tag>
      );
    }
    case "paragraph":
      return (
        <p className="md-paragraph">
          <InlineView nodes={block.content} />
        </p>
      );
    case "code":
      return (
        <div className="md-code-block">
          {block.language ? <span className="md-code-language">{block.language}</span> : null}
          <pre className="md-code-pre">
            <code>
              <KeywordText text={block.text} />
            </code>
          </pre>
        </div>
      );
    case "list": {
      const items = block.items.map((item, index) => (
        <li className={item.checked === null ? undefined : "md-task"} key={index}>
          {item.checked === null ? null : (
            <span aria-hidden="true" className="md-task-mark">
              {item.checked ? "☑" : "☐"}
            </span>
          )}
          <ListItemBlocks blocks={item.blocks} headingIds={headingIds} tight={block.tight} />
        </li>
      ));
      return block.ordered ? (
        <ol className="md-list" start={block.start === 1 ? undefined : block.start}>
          {items}
        </ol>
      ) : (
        <ul className="md-list">{items}</ul>
      );
    }
    case "quote":
      return (
        <blockquote className="md-quote">
          <BlockList blocks={block.blocks} headingIds={headingIds} />
        </blockquote>
      );
    case "table":
      return (
        <div className="md-table-wrap">
          <table className="md-table">
            <thead>
              <tr>
                {block.head.map((cell, index) => (
                  <th className={alignClass(block.align[index] ?? null)} key={index}>
                    <InlineView nodes={cell} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.map((cell, cellIndex) => (
                    <td className={alignClass(block.align[cellIndex] ?? null)} key={cellIndex}>
                      <InlineView nodes={cell} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "rule":
      return <hr className="md-rule" />;
    case "footnotes":
      return (
        <section className="md-footnotes">
          <ol>
            {block.items.map((item) => (
              <li key={item.label}>
                <InlineView nodes={item.content} />
              </li>
            ))}
          </ol>
        </section>
      );
  }
}

/**
 * CommonMark 的 tight 只省略列表项**直接子段落**的 `<p>`；引用、嵌套列表等容器里
 * 的段落仍走普通块渲染，并由那些容器自己的 tight 状态决定。
 */
function ListItemBlocks({ blocks, headingIds, tight }: { blocks: Block[]; headingIds: HeadingIds; tight: boolean }) {
  return (
    <>
      {blocks.map((block, index) =>
        tight && block.kind === "paragraph" ? (
          <InlineView nodes={block.content} key={index} />
        ) : (
          <BlockView block={block} headingIds={headingIds} key={index} />
        ),
      )}
    </>
  );
}

function BlockList({ blocks, headingIds }: { blocks: Block[]; headingIds: HeadingIds }) {
  return (
    <>
      {blocks.map((block, index) => (
        <BlockView block={block} headingIds={headingIds} key={index} />
      ))}
    </>
  );
}

function InlineView({ nodes }: { nodes: Inline[] }): ReactNode {
  return (
    <>
      {nodes.map((node, index) => {
        switch (node.kind) {
          case "text":
            return <KeywordText key={index} text={node.text} />;
          case "code":
            return (
              <code className="md-code-inline" key={index}>
                <KeywordText text={node.text} />
              </code>
            );
          case "strong":
            return (
              <strong key={index}>
                <InlineView nodes={node.children} />
              </strong>
            );
          case "emphasis":
            return (
              <em key={index}>
                <InlineView nodes={node.children} />
              </em>
            );
          case "strike":
            return (
              <del key={index}>
                <InlineView nodes={node.children} />
              </del>
            );
          case "link":
            return <MarkdownLink href={node.href} key={index}><InlineView nodes={node.children} /></MarkdownLink>;
          case "image":
            return <RemoteMarkdownImage key={index} href={node.href} alt={node.alt} />;
          case "footnote":
            return (
              <sup className="md-footnote-ref" key={index}>
                [{node.label}]
              </sup>
            );
          case "break":
            return <br key={index} />;
        }
      })}
    </>
  );
}

/**
 * A link is still a `<button>`: Markdown must never create a navigable `<a href>` in
 * the app shell. `#fragment` links jump within the document (headings now carry ids),
 * while everything else goes through the restricted external opener. A failed open is
 * reported next to the link instead of becoming an unhandled rejection.
 */
function MarkdownLink({ href, children }: { href: string; children: ReactNode }) {
  const [notice, setNotice] = useState<string | null>(null);
  const target = markdownLinkTarget(href);
  if (target.kind === "anchor") {
    return (
      <>
        <button
          type="button"
          className="md-link"
          data-anchor={target.id}
          title={href}
          onClick={(event) => {
            // Scope the lookup to this rendered message: two replies can both have a
            // "结论" heading, and jump must land in the message that owns the link.
            const root = event.currentTarget.closest("[data-markdown-root]");
            const heading = root
              ? Array.from(root.querySelectorAll("[id]")).find((element) => element.id === target.id) ?? null
              : null;
            if (!heading) {
              setNotice("找不到页内锚点：" + target.id);
              return;
            }
            setNotice(null);
            heading.scrollIntoView({ behavior: "smooth", block: "start" });
          }}
        >
          {children}
        </button>
        {notice ? <span className="md-link-error" role="status">{notice}</span> : null}
      </>
    );
  }
  return (
    <>
      <button
        type="button"
        className="md-link"
        title={href}
        onClick={() => {
          void openMarkdownExternalLink(href).then((failure) => setNotice(failure));
        }}
      >
        {children}
      </button>
      {notice ? <span className="md-link-error" role="status">{notice}</span> : null}
    </>
  );
}

/**
 * A remote image is a network capability. Rendering Markdown is not user consent to
 * exercise it, so the default state contains no resource-bearing element at all.
 */
function RemoteMarkdownImage({ href, alt }: { href: string; alt: string }) {
  const [approvedHref, setApprovedHref] = useState<string | null>(null);
  const src = approvedRemoteImageSource(approvedHref, href);

  if (src !== null) {
    return (
      <img
        className="md-image"
        src={src}
        alt={alt}
        loading="lazy"
        decoding="async"
        referrerPolicy="no-referrer"
      />
    );
  }

  return (
    <button
      type="button"
      className="md-image-prompt"
      title={href}
      onClick={() => setApprovedHref(href)}
    >
      <span className="md-image-prompt-alt">{alt || "远程图片"}</span>
      <span className="md-image-prompt-action">加载远程图片</span>
    </button>
  );
}

export function approvedRemoteImageSource(
  approvedHref: string | null,
  href: string,
): string | null {
  return approvedHref === href ? href : null;
}

function clampLevel(level: number): keyof typeof HEADING_TAG {
  if (level <= 1) return 1;
  if (level >= 6) return 6;
  return level as keyof typeof HEADING_TAG;
}

function alignClass(align: ColumnAlign): string | undefined {
  return align ? `md-align-${align}` : undefined;
}
