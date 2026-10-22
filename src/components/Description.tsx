import { Fragment } from "react";

/**
 * Renders the small Markdown subset used in option descriptions: paragraphs,
 * `code`, *emphasis* and roles like {command}`nvim` or {env}`EDITOR`.
 */
export function Description({ text }: { text: string }) {
  const paragraphs = text.trim().split(/\n\s*\n/);
  return (
    <div className="description">
      {paragraphs.map((p, i) => (
        <p key={i}>{inline(p.replace(/\s*\n\s*/g, " "))}</p>
      ))}
    </div>
  );
}

function inline(text: string) {
  const parts = text.split(/(\{\w+\}`[^`]*`|`[^`]*`|\*[^*\s][^*]*\*)/g);
  return parts.map((part, i) => {
    const role = part.match(/^\{\w+\}`([^`]*)`$/) ?? part.match(/^`([^`]*)`$/);
    if (role) return <code key={i}>{role[1]}</code>;
    const em = part.match(/^\*([^*]+)\*$/);
    if (em) return <em key={i}>{em[1]}</em>;
    return <Fragment key={i}>{part}</Fragment>;
  });
}
