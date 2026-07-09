// Cost honesty (journey 03): the label set is hard cap / soft limit / estimate /
// unknown, and they must not be conflated. A run-step limit truly bounds one
// activation's model/tool loop — a HARD CAP. A throttle (tokens/hour, max
// concurrent) only SLOWS an agent — a SOFT LIMIT, not an activation cap. Keeping
// them in separate lists is what lets the UI separate hard limits from estimates.
export function costSummary(profile: any, fallbackModel = '') {
  const model = profile?.model ?? fallbackModel;
  const turns = profile?.max_turns;
  const autonomy = profile?.autonomy ?? 'off';
  const hardCaps = [];
  if (turns) hardCaps.push(`${turns} run steps`);
  const softLimits = [];
  const throttle = profile?.throttle ?? {};
  for (const [name, t] of Object.entries(throttle) as any) {
    if (t?.llm_tokens_per_hour) softLimits.push(`${name}: ${t.llm_tokens_per_hour} tokens/hour`);
    if (t?.max_concurrent) softLimits.push(`${name}: ${t.max_concurrent} concurrent`);
  }
  const parts = [];
  if (hardCaps.length) parts.push('hard cap set');
  if (softLimits.length) parts.push('soft limit set');
  return {
    model: model || 'provider default',
    autonomy,
    hardCaps,
    softLimits,
    label: parts.length ? parts.join(' · ') : 'no limits set yet',
  };
}

export function autonomyConsequence(level = 'off') {
  switch (level) {
    case 'manual':
      return 'Agent setting changes can be prepared, but you still confirm before they take effect.';
    case 'assisted':
      return 'Low-risk agent setting changes may be accepted automatically; new add-ons and sandbox changes still ask you.';
    case 'autonomous':
      return 'This agent may accept its own routine setting changes without asking; high-risk changes still need you.';
    case 'off':
    default:
      return 'This agent cannot accept its own setting changes; every change waits for you.';
  }
}

export function modelCostHint(model = '') {
  const m = model.toLowerCase();
  if (!m) return 'cost/performance: unknown until a model is chosen';
  if (/haiku|mini|small|cheap|fast/.test(m)) return 'cost/performance: cheap';
  if (/sonnet|balanced|medium/.test(m)) return 'cost/performance: balanced';
  if (/opus|gpt-5|large|pro|max|power/.test(m)) return 'cost/performance: powerful';
  return 'cost/performance: unknown';
}
