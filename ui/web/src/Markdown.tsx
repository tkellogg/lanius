import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';

// Chat message renderer (docs/handoffs/platform-trust.md M4, journey 07).
// Agent messages are markdown (react-markdown + remark-gfm); external links open
// in a new tab. Raw HTML is gated on the platform trust level:
//   full    → rehype-raw is on, so an agent's raw HTML renders as real interface
//             elements (small UI and forms that continue a conversation).
//   reduced → rehype-raw is off, so HTML is shown as escaped text — the safe
//             behavior for a shared or remote machine (no cross-site scripting).
// The gate is the SINGLE decision point: the caller passes `allowHtml`, computed
// from /api/status `trust` (full ⇒ true). When trust is unknown (status not yet
// loaded) the caller defaults to false, so raw HTML never renders unguarded.
export default function Markdown({ text, allowHtml }: { text: string; allowHtml?: boolean }) {
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
