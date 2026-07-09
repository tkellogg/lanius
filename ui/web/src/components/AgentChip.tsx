// Deterministic per-agent identity chip: a small monogram in a bordered box.
// The brand is disciplined about color — the thorn (red) must always be the
// loudest thing on screen — so chips are NOT a rainbow. Each agent gets only a
// whisper of per-name hue within a narrow cool grey-blue band; the lightness
// (and thus contrast) comes from the theme via CSS, not a hardcoded hex.
function agentHue(name: string) {
  let h = 0;
  for (const c of String(name).toLowerCase()) h = (h * 31 + c.charCodeAt(0)) | 0;
  return 198 + (Math.abs(h) % 42); // 198–239: cool blue-grey, never warm
}
function AgentChip({ name, size = 'sm' as 'sm' | 'md' | 'lg', className = '' }: { name: string; size?: 'sm' | 'md' | 'lg'; className?: string }) {
  const mono = String(name).trim().slice(0, 2).toUpperCase() || '??';
  return <span className={`agent-chip agent-chip-${size}${className ? ` ${className}` : ''}`} style={{ ['--chip-h' as any]: agentHue(name) }} aria-hidden="true">{mono}</span>;
}

export default AgentChip;
