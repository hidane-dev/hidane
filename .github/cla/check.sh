#!/usr/bin/env bash
# The cla check (.github/workflows/cla.yml, docs/cla/README.md). Every author of a pull request's
# commits, and its author, must have agreed to the Individual CLA or be a Designated Contributor
# under a Corporate CLA, unless exempt (.github/cla/exempt, accounts ending in [bot]). When an
# author posts the statement, the agreement is recorded on the cla-signatures branch. Then the
# `cla` status is set on the pull request's head, and one comment on it is kept up to date.
#
# Environment: GH_TOKEN, REPO (owner/name) and PR (number); for a comment, COMMENT_BODY,
# COMMENTER_LOGIN, COMMENTER_ID and COMMENT_URL. DRY_RUN=1 prints what would be written instead.
set -euo pipefail

branch=cla-signatures
document=docs/cla/individual.md
version=1.0
statement='I have read the hidane Individual CLA 1.0 and I agree to it.'
marker='<!-- hidane-cla -->'
readme="https://github.com/$REPO/blob/main/docs/cla/README.md"
exempt_file=${EXEMPT_FILE:-.github/cla/exempt}

# Writes go through here, so that a dry run writes nothing.
write() {
  if [[ "${DRY_RUN:-}" == 1 ]]; then
    echo "dry run: gh api $*" >&2
    if [[ " $* " == *" --input - "* ]]; then cat >&2; fi
  else
    gh api "$@"
  fi
}

# A JSON file of the cla-signatures branch, or $2 when the branch or the file is missing; sets
# $file_sha to the file's blob, empty when it is missing.
read_record() {
  local response
  if response=$(gh api "repos/$REPO/contents/$1?ref=$branch" 2>/dev/null); then
    file_sha=$(jq -r .sha <<<"$response")
    jq -r .content <<<"$response" | base64 -d
  else
    file_sha=
    echo "$2"
  fi
}

pr=$(gh api "repos/$REPO/pulls/$PR")
if [[ $(jq -r .state <<<"$pr") != open ]]; then
  echo "#$PR is not open"
  exit 0
fi
head=$(jq -r .head.sha <<<"$pr")

# Who wrote the pull request: its author and each commit's author, by GitHub account. A commit
# whose author email is linked to no account cannot be attributed to anyone.
commits=$(gh api --paginate "repos/$REPO/pulls/$PR/commits" \
  --jq '.[] | {sha: .sha[0:7], login: .author.login, id: .author.id, name: .commit.author.name}' | jq -s .)
exempt=$(grep -v -e '^#' -e '^[[:space:]]*$' "$exempt_file" | jq -R . | jq -s .)
authors=$(jq -c --argjson pr "$pr" --argjson exempt "$exempt" '
  [{login: $pr.user.login, id: $pr.user.id}] + [.[] | select(.id != null) | {login, id}]
  | unique_by(.id)
  | map(select((.login | endswith("[bot]")) or (.login as $l | $exempt | index($l)) | not))' <<<"$commits")
unlinked=$(jq -c '[.[] | select(.id == null) | {sha, name}]' <<<"$commits")

individual=$(read_record individual.json '{"signatures": []}')
individual_sha=$file_sha
corporate=$(read_record corporate.json '{"agreements": []}')
covered() {
  jq -c --argjson individual "$individual" --argjson corporate "$corporate" '
    ([$individual.signatures[].id] + [$corporate.agreements[].designated[] | select(.removed == null) | .id]) as $ids
    | map(select(.id as $id | $ids | index($id) | not))' <<<"$authors"
}
unsigned=$(covered)

# An author who posts the statement agrees for themselves.
body=$(jq -rn --arg body "${COMMENT_BODY:-}" '$body | gsub("^\\s+|\\s+$"; "")')
if [[ "$body" == "$statement" ]] && jq -e --argjson id "$COMMENTER_ID" 'any(.id == $id)' <<<"$unsigned" >/dev/null; then
  record=$(jq -n --arg login "$COMMENTER_LOGIN" --argjson id "$COMMENTER_ID" --arg version "$version" \
    --arg sha256 "$({ sha256sum "$document" 2>/dev/null || shasum -a 256 "$document"; } | cut -d' ' -f1)" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson pr "$PR" --arg comment "$COMMENT_URL" \
    '{login: $login, id: $id, agreement: "individual", version: $version, document_sha256: $sha256,
      signed_at: $at, pull_request: $pr, comment: $comment}')
  if ! gh api "repos/$REPO/git/ref/heads/$branch" >/dev/null 2>&1; then
    # The branch holds the records only: it starts from a commit of its own.
    tree=$(jq -n --arg readme "$(cat <<EOF
# hidane CLA records

Agreements to the hidane Contributor License Agreements, recorded by the cla check
(.github/workflows/cla.yml on main). See docs/cla/README.md on main.
EOF
)" '{tree: [{path: "README.md", mode: "100644", type: "blob", content: $readme}]}' | write "repos/$REPO/git/trees" --input - --jq .sha)
    commit=$(jq -n --arg tree "$tree" '{message: "Start the record of CLA agreements", tree: $tree, parents: []}' \
      | write "repos/$REPO/git/commits" --input - --jq .sha)
    write "repos/$REPO/git/refs" -f ref="refs/heads/$branch" -f sha="$commit" >/dev/null
  fi
  for attempt in 1 2 3; do
    updated=$(jq --argjson record "$record" '.signatures += [$record]' <<<"$individual")
    if jq -n --arg message "$COMMENTER_LOGIN agrees to the Individual CLA $version in #$PR" \
      --arg content "$(printf '%s\n' "$updated" | base64 | tr -d '\n')" --arg branch "$branch" --arg sha "$individual_sha" \
      '{message: $message, content: $content, branch: $branch} + (if $sha == "" then {} else {sha: $sha} end)' \
      | write -X PUT "repos/$REPO/contents/individual.json" --input - >/dev/null; then
      individual=$updated
      break
    fi
    # Someone else recorded an agreement at the same time: read the file again.
    [[ $attempt == 3 ]] && exit 1
    individual=$(read_record individual.json '{"signatures": []}')
    individual_sha=$file_sha
  done
  unsigned=$(covered)
fi

missing=$(jq length <<<"$unsigned")
orphans=$(jq length <<<"$unlinked")
if ((missing + orphans > 0)); then
  state=failure
  description="Waiting for $missing author(s) to agree to the CLA"
  ((orphans > 0)) && description="$description; $orphans commit(s) not linked to a GitHub account"
else
  state=success
  description="Every author has agreed to the CLA"
  [[ $(jq length <<<"$authors") == 0 ]] && description="No agreement needed: maintainers and bots only"
fi
write -X POST "repos/$REPO/statuses/$head" -f state="$state" -f context=cla \
  -f description="$description" -f target_url="$readme" >/dev/null
echo "cla: $state ($description)"

# One comment, kept up to date, once someone outside the exempt accounts is involved.
existing=$(gh api --paginate "repos/$REPO/issues/$PR/comments" \
  --jq ".[] | select(.user.login == \"github-actions[bot]\" and (.body | contains(\"$marker\"))) | .id" | head -n1)
if [[ $state == success ]]; then
  [[ -z "$existing" ]] && exit 0
  message="$marker
Every author of this pull request has agreed to the hidane [Contributor License Agreement]($readme). Thank you!"
else
  message="$marker
Thank you for your pull request. hidane accepts contributions under its [Contributor License Agreement]($readme): you keep the copyright in your contribution, and the project receives a license to use, distribute, sublicense and relicense it."
  if ((missing > 0)); then
    message="$message

$(jq -r 'map("@" + .login) | join(", ")' <<<"$unsigned") still need to agree to the [Individual CLA](https://github.com/$REPO/blob/main/$document). Each of them can agree by posting this comment, exactly as written:

\`\`\`
$statement
\`\`\`

If you contribute as part of your work for a company, it may need to sign the Corporate CLA instead; see the link above."
  fi
  if ((orphans > 0)); then
    message="$message

These commits are authored with an email address that is linked to no GitHub account, so their authors cannot be identified: $(jq -r 'map("`" + .sha + "` (" + .name + ")") | join(", ")' <<<"$unlinked"). Author them with an email address linked to your GitHub account, or ask a maintainer."
  fi
  message="$message

Post \`recheck\` to run the check again."
fi
if [[ -n "$existing" ]]; then
  write -X PATCH "repos/$REPO/issues/comments/$existing" -f body="$message" >/dev/null
else
  write "repos/$REPO/issues/$PR/comments" -f body="$message" >/dev/null
fi
