import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';

// Chat message renderer (docs/handoffs/platform-trust.md M4, journeys 07,
// docs/handoffs/html-messages.md). Agent messages are markdown (react-markdown +
// remark-gfm); external links open in a new tab. Whether raw HTML becomes live
// DOM is gated on ONE thing: the platform trust level (`allowHtml`, computed
// from /api/status `trust` — full ⇒ true; unknown ⇒ false so HTML never renders
// unguarded). `format` records the agent's DELIBERATE intent and only changes
// the render *shape*, never the gate:
//   format="html" + full trust → the whole body is an HTML fragment (a form, a
//             button-bar): render it as HTML directly, skipping markdown block
//             processing so a <form>/<table> isn't mangled by paragraph rules.
//   markdown (default)         → render as markdown; at full trust inline raw
//             HTML in it still renders (rehype-raw) — the "small touches" case.
//   reduced trust              → HTML is shown as escaped text either way (no
//             live element), the safe behavior for a shared/remote machine.
export default function Markdown({
  text,
  allowHtml,
  format,
}: {
  text: string;
  allowHtml?: boolean;
  format?: string;
}) {
  // Deliberate whole-body HTML. `format` alone never unlocks live DOM —
  // `allowHtml` (trust===full) is still the sole gate:
  //   full    → render the fragment as real DOM, no markdown block-processing so
  //             a <form>/<table> isn't mangled by paragraph rules.
  //   reduced → show the markup as visible escaped text (JSX escapes it) so the
  //             person sees exactly what the agent sent, but nothing runs. (Plain
  //             react-markdown would silently DROP raw HTML, not escape it, so the
  //             whole-body case renders it explicitly here.)
  if (format === 'html') {
    return allowHtml ? (
      <div className="msg-html" dangerouslySetInnerHTML={{ __html: text }} />
    ) : (
      <div className="msg-html-escaped">{text}</div>
    );
  }
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      rehypePlugins={allowHtml ? [rehypeRaw] : []}
      components={{
        a: ({ node, ...props }) => <a {...props} target="_blank" rel="noopener noreferrer" />,
      }}
    >
      {text}
    </ReactMarkdown>
  );
}
