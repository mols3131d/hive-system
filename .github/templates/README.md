---
description: "Choose GitHub issue, pull request, comment and review authoring templates."
---

# GitHub Agent Authoring Templates

GitHub recognizes [Issue Forms](../ISSUE_TEMPLATE/) and the [Markdown Issue template](../ISSUE_TEMPLATE/issue.md) in its web UI, plus the [PR template](../pull_request_template.md). The comment/review templates here are **authoring aids**. API/Plugin authors must explicitly load and apply templates; Issue Form validation does not apply to API-created Issues.

## Select a template

| GitHub surface | Source |
| --- | --- |
| Issue (specific report or proposal, web UI) | [Native Issue forms](../ISSUE_TEMPLATE/) |
| Issue (general request or API/Plugin) | [General Issue template](../ISSUE_TEMPLATE/issue.md) |
| Pull request | [Native PR template](../pull_request_template.md) |
| Issue or PR conversation comment | [Conversation comment](comment.md) |
| PR review | [Review](pull-request-review.md) |
| PR inline review comment | [Inline review](pull-request-inline-comment.md) |

Comment/review authoring aids use illustrative `{{ value }}`, `{% if ... %}`, and `{% for ... %}` notation, not an installed template engine. Markdown templates use visible italic parenthetical writing guidance. Follow the relevant guidance, then remove it along with unused sections, examples, placeholders, and authoring notation before posting. Do not duplicate GitHub-native statuses or review decisions.
