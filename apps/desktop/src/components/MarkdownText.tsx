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
 * 2. **链接只在外部浏览器打开。** 渲染层使用 opener 能力允许的 `http(s)` / `mailto`
 *    URL，不让 WebView 导航离开应用。未知 scheme 在解析阶段已经是普通文字。
 * 3. **图片不携带来源信息。** 只有解析器认可的 `http(s)` URL 会进入 `<img>`，
 *    并使用 `no-referrer`；HTML 仍然永远只是文字。
 *
 * 关键词着色（`KeywordText`）在正文里全量保留 —— Markdown 只负责结构，哪些词是
 * 错误、路径还是标识符，与原来一样由 token 规则决定。
 */

import type { ReactNode } from "react";

import { openExternalUrl } from "../lib/external.js";
import { parseMarkdown, type Block, type ColumnAlign, type Inline } from "../lib/markdown.js";
import { KeywordText } from "./KeywordText.js";

const HEADING_TAG = { 1: "h1", 2: "h2", 3: "h3", 4: "h4", 5: "h5", 6: "h6" } as const;

export function MarkdownText({ text, className }: { text: string; className?: string }) {
  const blocks = parseMarkdown(text);
  return (
    <div className={className}>
      {blocks.map((block, index) => (
        <BlockView block={block} key={index} />
      ))}
    </div>
  );
}

function BlockView({ block }: { block: Block }) {
  switch (block.kind) {
    case "heading": {
      const Tag = HEADING_TAG[clampLevel(block.level)];
      return (
        <Tag className="md-heading">
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
          <ListItemBlocks blocks={item.blocks} tight={block.tight} />
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
          <BlockList blocks={block.blocks} />
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
function ListItemBlocks({ blocks, tight }: { blocks: Block[]; tight: boolean }) {
  return (
    <>
      {blocks.map((block, index) =>
        tight && block.kind === "paragraph" ? (
          <InlineView nodes={block.content} key={index} />
        ) : (
          <BlockView block={block} key={index} />
        ),
      )}
    </>
  );
}

function BlockList({ blocks }: { blocks: Block[] }) {
  return (
    <>
      {blocks.map((block, index) => (
        <BlockView block={block} key={index} />
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
            return (
              <button
                type="button"
                className="md-link"
                key={index}
                title={node.href}
                onClick={() => void openExternalUrl(node.href)}
              >
                <InlineView nodes={node.children} />
              </button>
            );
          case "image":
            return (
              <img
                className="md-image"
                key={index}
                src={node.href}
                alt={node.alt}
                loading="lazy"
                decoding="async"
                referrerPolicy="no-referrer"
              />
            );
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

function clampLevel(level: number): keyof typeof HEADING_TAG {
  if (level <= 1) return 1;
  if (level >= 6) return 6;
  return level as keyof typeof HEADING_TAG;
}

function alignClass(align: ColumnAlign): string | undefined {
  return align ? `md-align-${align}` : undefined;
}
