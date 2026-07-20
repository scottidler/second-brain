# Session thread: slack-cli release-promote (2 sessions)

USER: The release-promote workflow for slack-cli keeps failing at the tag step. It tags on the feature branch, then the squash-merge rewrites the SHA and the tag is orphaned. How do we fix this so the tag always lands on the merged commit?

ASSISTANT: The root cause is tagging before the merge. A tag created on a feature branch points at a commit the squash-merge discards, so the tag references a SHA that never reaches main. The fix is to split the bump from the tag: run the version bump inside the feature PR with no tag, merge, then tag the merged commit on main. That is exactly what `bump --no-tag` on the branch plus `bump --tag-only` after merge gives you.

USER: But the repo has branch protection on main, so I can't push a tag directly either.

ASSISTANT: Branch protection blocks direct branch pushes, not lightweight tag pushes, unless a ruleset also covers tags. Check both gates: `gh api repos/OWNER/REPO/branches/main/protection` and `gh api repos/OWNER/REPO/rules/branches/main`. If only classic protection exists, `git push origin vX.Y.Z` still works after the branch is merged. Decision: gate the tag push on `HEAD == origin/main` so a rejected branch push can never let the tag escape.

USER: Good. Also the workflow used `git push --tags` which pushed the tag even when the branch push was rejected.

ASSISTANT: That is the orphan mechanism. `push.followTags`/`--tags` lands the tag independent of the branch push result. Never use `git push --tags` in release automation. Push the branch first with `&&` chaining, then push the tag by explicit name: `git push origin main && git push origin vX.Y.Z`. The `&&` means a rejected branch push aborts before the tag is pushed.

USER: One more: the CI tagged dev branches by accident during testing.

ASSISTANT: Add a guard that refuses to tag on any branch other than main/master. Tags belong only on the default branch; tagging a dev or feature branch produces a burnt tag after squash-merge. The gotcha to remember: a tag created on a branch is lost forever once the branch is squash-merged, because the SHA it points at is rewritten.
