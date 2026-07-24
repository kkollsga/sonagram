---
name: music_playlist_audit
description: "TRIGGER when evaluating an ordered playlist or before claiming one is ready. Use Sonagram's independent audit/explanation methods and report their metrics, issues, and aggression evidence status when active. SKIP subjective quality claims unsupported by the audit and track metadata."
references_tools: [music_audit_playlist, music_explain_playlist]
applies_when:
  tool_registered: music_audit_playlist
  graph_has_property: {node_type: Track, prop_name: is_canonical}
---

<!-- sonagram-curation-contract:v1 -->

# Audit is the acceptance gate

For library-curated results, require `exportable: true` and `audit.passed:
true`. For an existing order call `music_audit_playlist`, then
`music_explain_playlist` when diagnosis is useful. Include the original brief
when available so target count, duration, seed-relative, and categorical intent
are independently checked. CLI/Python parity is `audit`/`explain` and
`audit_playlist`/`explain_playlist`.

Check hard eligibility, duplicate Track/Song IDs, artist/album concentration,
artist spacing, mean and worst transition scores, duration, and arc error. If
output is poor despite passing, record the concrete defect as a Sonagram library
issue; do not compensate with private agent-only selection rules.

When aggression was explicitly requested, treat `aggression_unknown` as a hard
failure. Explanation evidence distinguishes `available`, `abstained`, `missing`,
`incompatible_model`, and `invalid_diagnostics`; a valid abstention is honest
analysis output, not a low score. Report the status and evidence support, and do
not reinterpret support as certainty or replace the missing rank with
`mood_aggressive`, `tension_index`, energy, or genre.
