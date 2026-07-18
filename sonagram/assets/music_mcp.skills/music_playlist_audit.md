---
name: music_playlist_audit
description: "TRIGGER when evaluating an ordered playlist or before claiming one is ready. Use Sonagram's independent audit/explanation methods and report their metrics/issues. SKIP subjective quality claims unsupported by the audit and track metadata."
references_tools: [music_library_profile]
applies_when:
  graph_has_property: {node_type: Track, prop_name: is_canonical}
---

<!-- sonagram-curation-contract:v1 -->

# Audit is the acceptance gate

For library-curated results, require `exportable: true` and `audit.passed:
true`. For an existing order run `sonagram audit --ids ... --format json`, then
`sonagram explain --ids ... --format json` when diagnosis is useful. Python
parity is `audit_playlist` / `explain_playlist`.

Check hard eligibility, duplicate Track/Song IDs, artist/album concentration,
artist spacing, mean and worst transition scores, duration, and arc error. If
output is poor despite passing, record the concrete defect as a Sonagram library
issue; do not compensate with private agent-only selection rules.
