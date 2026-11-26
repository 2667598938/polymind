import React from "react";

/**
 * 极简富文本渲染：只认 fenced code block 与行内 code，不引第三方 markdown 依赖。
 * 目的：Agent 输出里代码占比很高，必须保住等宽与换行。
 */
export default function RichText({ text }: { text: string }) {
  if (!text) return null;
  const segments = text.split("```");
  return (
    <>
      {segments.map((seg, i) => {
        if (i % 2 === 1) {
          const firstNewline = seg.indexOf("\n");
          const lang = firstNewline > -1 ? seg.slice(0, firstNewline).trim() : "";
          const body = firstNewline > -1 ? seg.slice(firstNewline + 1) : seg;
          return (
            <div className="code-block" key={i}>
              {lang && <span className="code-lang">{lang}</span>}
              <pre>{body.replace(/\n$/, "")}</pre>
            </div>
          );
        }
        if (!seg.trim()) return null;
        return (
          <p className="prose" key={i}>
            {renderInline(seg.replace(/^\n+|\n+$/g, ""))}
          </p>
        );
      })}
    </>
  );
}

function renderInline(text: string): React.ReactNode[] {
  return text.split(/(`[^`\n]+`)/g).map((chunk, i) => {
    if (chunk.startsWith("`") && chunk.endsWith("`") && chunk.length > 2) {
      return <code key={i}>{chunk.slice(1, -1)}</code>;
    }
    return <React.Fragment key={i}>{chunk}</React.Fragment>;
  });
}
